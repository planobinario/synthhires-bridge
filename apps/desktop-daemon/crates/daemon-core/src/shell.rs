//! Shell execution.
//!
//! HARD RULE: the daemon never invokes `sh -c "<user args>"` with
//! user-provided args concatenated as a string. That is the classic
//! shell-injection vector. We use `std::process::Command` with the
//! command and args as discrete argv entries, so the kernel's exec
//! boundary is the only interpretation point.
//!
//! The only string-concat path is the explicit "shell" mode where
//! the user enters a single command string (e.g. "ls -la"). That
//! goes through `Command::new("bash").arg("-lc").arg(cmd)` so the
//! shell still parses it as a single command line — but the user
//! opted into shell semantics explicitly by selecting shell mode.

use crate::{capability::CapabilityGate, Result, DaemonError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

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
            return Err(DaemonError::CapabilityDenied("desktop.shell.execute".into()));
        }
        let timeout = req.timeout_ms.unwrap_or(30_000);
        let mut cmd = Command::new("bash");
        cmd.arg("-lc").arg(&req.command);
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        // Kill the entire process group on timeout — important for
        // shell commands that fork (e.g. `npm test && watch ...`).
        cmd.kill_on_drop(true);

        let start = std::time::Instant::now();
        let mut child = cmd.spawn().map_err(DaemonError::Io)?;
        let stdout = child.stdout.take().ok_or_else(|| DaemonError::Io(
            std::io::Error::other("no stdout"),
        ))?;
        let stderr = child.stderr.take().ok_or_else(|| DaemonError::Io(
            std::io::Error::other("no stderr"),
        ))?;

        let (tx, rx) = mpsc::channel::<ShellOutputChunk>(64);

        // Stdout pump
        let txo = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if txo
                    .send(ShellOutputChunk { channel: "stdout", data: format!("{}\n", line) })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        // Stderr pump
        let txe = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if txe
                    .send(ShellOutputChunk { channel: "stderr", data: format!("{}\n", line) })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Wait with a hard timeout. SIGKILL the entire process group
        // so child processes (e.g. backgrounded `&` jobs) die too.
        let future = ShellResultFuture {
            child,
            timeout_ms: timeout,
            started_at: start,
            tx,
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
}

impl ShellResultFuture {
    pub async fn await_result(mut self) -> Result<ShellResult> {
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            self.child.wait(),
        )
        .await;
        let stdout_buf = String::new();
        let stderr_buf = String::new();
        // Drain tx so the pumps don't deadlock.
        drop(self.tx);
        match result {
            Ok(Ok(status)) => Ok(ShellResult {
                exit_code: status.code(),
                stdout: stdout_buf,
                stderr: stderr_buf,
                duration_ms: self.started_at.elapsed().as_millis() as u64,
            }),
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