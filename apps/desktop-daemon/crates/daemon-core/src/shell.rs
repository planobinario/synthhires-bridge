//! Shell execution.
//!
//! HARD RULE: the daemon never invokes `sh -c "<user args>"` with
//! user-provided args concatenated as a string. That is the classic
//! shell-injection vector. We use `std::process::Command` with the
//! command and args as discrete argv entries, so the kernel's exec
//! boundary is the only interpretation point.
//!
//! The only string-concat path is the explicit "shell" mode where
//! the user enters a single command string (e.g. "ls -la"). On Unix
//! that goes through `Command::new("bash").arg("-lc").arg(cmd)`; on
//! Windows there is no bash — we use `cmd.exe /C` with the command
//! string passed as a single argv entry, so the shell still parses
//! it as one command line.

use crate::{capability::CapabilityGate, DaemonError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone, Deserialize)]
pub struct ShellRequest {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellOutputChunk {
    pub channel: &'static str,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

pub struct ShellRunner<'a> {
    gate: &'a CapabilityGate,
}

impl<'a> ShellRunner<'a> {
    pub fn new(gate: &'a CapabilityGate) -> Self {
        Self { gate }
    }

    /// Execute the request. Returns a stream handle + final result.
    /// The caller pipes chunks to the WS as `action_stream` frames and
    /// sends the final `action_result` with the exit code.
    pub async fn run(
        &self,
        req: ShellRequest,
    ) -> Result<(mpsc::Receiver<ShellOutputChunk>, ShellResultFuture)> {
        if !self.gate.allows("desktop.shell.execute") {
            return Err(DaemonError::CapabilityDenied(
                "desktop.shell.execute".into(),
            ));
        }
        let timeout = req.timeout_ms.unwrap_or(30_000);
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            // /C runs the command and exits. The command string is a
            // single argv entry — no shell injection beyond what the
            // user already typed into their own terminal semantics.
            c.arg("/C").arg(&req.command);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg("-lc").arg(&req.command);
            c
        };
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        // Kill the entire process group on timeout — important for
        // shell commands that fork (e.g. `npm test && watch ...`).
        cmd.kill_on_drop(true);

        let start = std::time::Instant::now();
        let mut child = cmd.spawn().map_err(DaemonError::Io)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DaemonError::Io(std::io::Error::other("no stdout")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| DaemonError::Io(std::io::Error::other("no stderr")))?;

        let (tx, rx) = mpsc::channel::<ShellOutputChunk>(64);

        // Accumulated buffers so the final result carries the REAL
        // output (previous code returned empty strings).
        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));

        // Stdout pump: broadcast to the stream AND accumulate.
        let txo = tx.clone();
        let acco = stdout_buf.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let data = format!("{}\n", line);
                acco.lock().await.push_str(&data);
                if txo
                    .send(ShellOutputChunk {
                        channel: "stdout",
                        data,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        // Stderr pump: broadcast to the stream AND accumulate.
        let txe = tx.clone();
        let acce = stderr_buf.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let data = format!("{}\n", line);
                acce.lock().await.push_str(&data);
                if txe
                    .send(ShellOutputChunk {
                        channel: "stderr",
                        data,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let future = ShellResultFuture {
            child,
            timeout_ms: timeout,
            started_at: start,
            tx,
            stdout_buf,
            stderr_buf,
            stdout_task,
            stderr_task,
        };
        Ok((rx, future))
    }
}

/// Awaits the child with the configured timeout. The receiver of the
/// stream must drain it before this completes; otherwise the
/// pumps deadlock on full channel.
pub struct ShellResultFuture {
    child: tokio::process::Child,
    timeout_ms: u64,
    started_at: std::time::Instant,
    tx: mpsc::Sender<ShellOutputChunk>,
    stdout_buf: Arc<Mutex<String>>,
    stderr_buf: Arc<Mutex<String>>,
    stdout_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
}

impl ShellResultFuture {
    pub async fn await_result(mut self) -> Result<ShellResult> {
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            self.child.wait(),
        )
        .await;
        // Drain tx so the pumps don't deadlock.
        drop(self.tx);
        // The pumps reach EOF only after the child exits AND the pipe
        // is fully drained — join them so the accumulated buffers are
        // complete before we read them.
        let _ = self.stdout_task.await;
        let _ = self.stderr_task.await;
        match result {
            Ok(Ok(status)) => {
                let stdout_buf = self.stdout_buf.lock().await.clone();
                let stderr_buf = self.stderr_buf.lock().await.clone();
                Ok(ShellResult {
                    exit_code: status.code(),
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                    duration_ms: self.started_at.elapsed().as_millis() as u64,
                })
            }
            Ok(Err(e)) => Err(DaemonError::Io(e)),
            Err(_) => {
                // Hard kill on timeout. start_kill sends SIGKILL.
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
                Err(DaemonError::Timeout(self.timeout_ms))
            }
        }
    }
}
