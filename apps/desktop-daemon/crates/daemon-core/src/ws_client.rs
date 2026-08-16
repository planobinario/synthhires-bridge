//! WebSocket client loop.
//!
//! dials the backend's /api/devices/ws endpoint with the deviceToken
//! from the OS keyring. Maintains the hello/heartbeat protocol,
//! routes incoming action_request frames to the local capability
//! gate + shell runner / fs ops, and ships the results back over
//! the same WS.
//!
//! Reconnect strategy: exponential backoff capped at 30s, jittered
//! to avoid thundering herd. The first hello after a reconnect
//! triggers a `resume` frame from the server so any pending
//! action_result the device buffered gets re-delivered.

use crate::{
    capability::{CapabilityGate, GateDecision, ScopeSnapshot},
    chat_store::ChatStore,
    consent::{ConsentAnswer, ConsentBroker, ConsentPrompt},
    DaemonError, Result,
};
use daemon_protocol::{parse_chat_push_params, BridgeFrame, HelloFrame, PROTOCOL_VERSION};
use futures_util::{SinkExt, StreamExt};
use sha2::Digest;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};

pub struct WsClient {
    backend_url: String,
    token: String,
    _device_id: String,
    fingerprint: String,
    device_kind: &'static str,
    device_name: String,
    gate: std::sync::Arc<Mutex<CapabilityGate>>,
    chat_store: Arc<ChatStore>,
    consent: std::sync::Arc<ConsentBroker>,
}

impl WsClient {
    // The constructor had 7 args before the chat archive landed; adding
    // one more tips it past clippy's default. The alternatives (builder,
    // config struct) are churn for three call sites — keep the flat shape
    // and document the contract instead.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend_url: impl Into<String>,
        token: impl Into<String>,
        device_id: impl Into<String>,
        fingerprint: impl Into<String>,
        device_kind: &'static str,
        device_name: impl Into<String>,
        gate: CapabilityGate,
        chat_store: Arc<ChatStore>,
        consent: std::sync::Arc<ConsentBroker>,
    ) -> Self {
        Self {
            backend_url: backend_url.into(),
            token: token.into(),
            _device_id: device_id.into(),
            fingerprint: fingerprint.into(),
            device_kind,
            device_name: device_name.into(),
            gate: std::sync::Arc::new(Mutex::new(gate)),
            chat_store,
            consent,
        }
    }

    /// Run the connection loop. Returns only on unrecoverable error
    /// (auth failure, protocol mismatch). Network blips are absorbed
    /// internally with exponential backoff.
    pub async fn run(&self) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.connect_once().await {
                Ok(()) => {
                    // Graceful close; treat as a normal reconnect.
                    tracing::info!("WS closed cleanly; reconnecting in {:?}", backoff);
                }
                Err(crate::DaemonError::Protocol(msg))
                    if msg.contains("auth_failed") || msg.contains("revoked") =>
                {
                    return Err(crate::DaemonError::Protocol(msg));
                }
                Err(e) => {
                    tracing::warn!("WS error: {e}; reconnecting in {:?}", backoff);
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
            // Add 0-1s jitter
            let jitter = Duration::from_millis(rand::random::<u64>() % 1000);
            tokio::time::sleep(jitter).await;
        }
    }

    async fn connect_once(&self) -> Result<()> {
        tracing::info!("Attempting WS connection to URL: {}", self.backend_url);
        let mut req = self
            .backend_url
            .clone()
            .into_client_request()
            .map_err(|e| crate::DaemonError::Ws(format!("into_client_request: {e}")))?;
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            format!("bearer.{}", self.token).parse().map_err(
                |e: http::header::InvalidHeaderValue| {
                    crate::DaemonError::Ws(format!("invalid header: {e}"))
                },
            )?,
        );
        // Defense in depth: the DO also checks this header against the
        // token hash before accepting the socket.
        let token_hash = hex::encode(sha2::Sha256::digest(self.token.as_bytes()));
        req.headers_mut().insert(
            "x-bridge-token-hash",
            token_hash.parse().map_err(|e: http::header::InvalidHeaderValue| {
                crate::DaemonError::Ws(format!("invalid header: {e}"))
            })?,
        );
        let (mut ws, _resp) = connect_async(req)
            .await
            .map_err(|e| crate::DaemonError::Ws(format!("connect: {e}")))?;
        // Send hello
        let hello = BridgeFrame::Hello(HelloFrame {
            v: PROTOCOL_VERSION,
            token_hash,
            fingerprint: self.fingerprint.clone(),
            device_kind: match self.device_kind {
                "desktop" => daemon_protocol::DeviceKind::Desktop,
                _ => daemon_protocol::DeviceKind::Mobile,
            },
            device_name: self.device_name.clone(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        ws.send(Message::Text(serde_json::to_string(&hello)?))
            .await
            .map_err(|e| crate::DaemonError::Ws(format!("send hello: {e}")))?;

        // Read hello_ack (first frame MUST be hello_ack per protocol).
        let first = ws
            .next()
            .await
            .ok_or_else(|| crate::DaemonError::Protocol("ws closed before hello_ack".into()))?
            .map_err(|e| crate::DaemonError::Ws(format!("recv hello_ack: {e}")))?;
        let ack: BridgeFrame = match first {
            Message::Text(t) => serde_json::from_str(&t)?,
            _ => return Err(crate::DaemonError::Protocol("hello_ack not text".into())),
        };
        let ack = match ack {
            BridgeFrame::HelloAck(ack) => ack,
            BridgeFrame::Error(e) => {
                return Err(crate::DaemonError::Protocol(format!(
                    "{}: {}",
                    e.code, e.message
                )));
            }
            _ => return Err(crate::DaemonError::Protocol("expected hello_ack".into())),
        };
        // Update scope cache
        let mut g = self.gate.lock().await;
        let scopes = ScopeSnapshot::from(&ack.scopes);
        *g = CapabilityGate::new(scopes);
        drop(g);

        // Loop: heartbeat every 30s, dispatch incoming actions.
        let mut heartbeat = tokio::time::interval(Duration::from_millis(30_000));
        loop {
            tokio::select! {
                Some(msg) = ws.next() => {
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => return Err(crate::DaemonError::Ws(format!("recv: {e}"))),
                    };
                    match msg {
                        Message::Text(t) => {
                            let frame: BridgeFrame = serde_json::from_str(&t)?;
                            self.handle_frame(&mut ws, frame).await?;
                        }
                        Message::Close(c) => {
                            tracing::info!("server closed ws: {:?}", c);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ = heartbeat.tick() => {
                    let ping = BridgeFrame::Heartbeat(daemon_protocol::HeartbeatFrame {
                        v: PROTOCOL_VERSION,
                        t: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    });
                    ws.send(Message::Text(serde_json::to_string(&ping)?)).await
                        .map_err(|e| crate::DaemonError::Ws(format!("send heartbeat: {e}")))?;
                }
            }
        }
    }

    async fn handle_frame(
        &self,
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        frame: BridgeFrame,
    ) -> Result<()> {
        match frame {
            BridgeFrame::ActionRequest(req) => {
                if req.capability == "sync.chat.push" {
                    self.handle_chat_push(ws, req).await
                } else {
                    self.handle_action(ws, req).await
                }
            }
            BridgeFrame::ScopeUpdate(upd) => {
                let mut g = self.gate.lock().await;
                let snap = ScopeSnapshot::from(&upd.scopes);
                *g = CapabilityGate::new(snap);
                tracing::info!("scope updated by server");
                Ok(())
            }
            BridgeFrame::Revoke(rev) => {
                tracing::warn!("revoked by server: {}", rev.reason);
                Err(crate::DaemonError::Protocol("revoked".into()))
            }
            BridgeFrame::Error(e) => {
                tracing::error!("server error: {}: {}", e.code, e.message);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn handle_action(
        &self,
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        req: daemon_protocol::ActionRequestFrame,
    ) -> Result<()> {
        // 1. Audit Log (Local inmutable record)
        let config_dir = directories::ProjectDirs::from("com", "synthhires", "bridge")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join("synthhires-bridge")
            });
        let audit_log_path = config_dir.join("audit.log");
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let params_str = serde_json::to_string(&req.params).unwrap_or_default();
        let log_entry = format!(
            "[{}] CAPABILITY: {} PARAMS: {}\n",
            timestamp, req.capability, params_str
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&audit_log_path)
        {
            use std::io::Write;
            let _ = file.write_all(log_entry.as_bytes());
        }

        // 2. Capability Gate Check
        let g = self.gate.lock().await;
        let allowed = g.allows(&req.capability);
        drop(g);
        if !allowed {
            let result = daemon_protocol::ActionResultFrame {
                v: PROTOCOL_VERSION,
                id: req.id,
                ok: false,
                output: None,
                error: Some(daemon_protocol::ActionError {
                    code: "capability_not_granted".into(),
                    message: format!(
                        "La capability '{}' no está concedida a este dispositivo.",
                        req.capability
                    ),
                }),
                duration_ms: 0,
            };
            ws.send(Message::Text(serde_json::to_string(
                &BridgeFrame::ActionResult(result),
            )?))
            .await?;
            return Ok(());
        }

        // 3. Hard-Stops for Destructive Commands (DPI)
        if req.capability == "desktop.shell.execute" {
            let cmd = req
                .params
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let dangerous_patterns = [
                "sudo ",
                "rm -rf",
                "del /f /s /q",
                "mkfs",
                "chmod -R 777",
                "chown -R",
            ];
            for pattern in dangerous_patterns {
                if cmd.contains(pattern) {
                    tracing::error!("Hard-stop triggered for dangerous command: {}", cmd);
                    let result = daemon_protocol::ActionResultFrame {
                        v: PROTOCOL_VERSION,
                        id: req.id,
                        ok: false,
                        output: None,
                        error: Some(daemon_protocol::ActionError {
                            code: "hard_stop_blocked".into(),
                            message: format!("El comando contiene patrones destructivos prohibidos por seguridad ('{}').", pattern),
                        }),
                        duration_ms: 0,
                    };
                    ws.send(Message::Text(serde_json::to_string(
                        &BridgeFrame::ActionResult(result),
                    )?))
                    .await?;
                    return Ok(());
                }
            }

            // Consent: skip only when the server explicitly trusts the
            // action (alwaysAllowPaths configured in the web device
            // panel). Everything else raises a native prompt through the
            // ConsentBroker — the daemon NEVER silently executes
            // arbitrary shell commands on the user's machine, and never
            // silently refuses them either.
            if !req.skip_consent_prompt {
                let target = req
                    .params
                    .get("path")
                    .or_else(|| req.params.get("cwd"))
                    .and_then(|v| v.as_str());
                let prompt = ConsentPrompt {
                    action_id: req.id.clone(),
                    capability: req.capability.clone(),
                    summary: cmd.to_string(),
                    path: target.map(|p| p.to_string()),
                };
                let mut rx = self.consent.ask(prompt);
                let answer = tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    &mut rx,
                )
                .await;
                let answer: ConsentAnswer = match answer {
                    Ok(Ok(a)) => a,
                    Ok(Err(_)) | Err(_) => ConsentAnswer::default(), // no answer = deny
                };
                if !answer.approved {
                    let result = daemon_protocol::ActionResultFrame {
                        v: PROTOCOL_VERSION,
                        id: req.id,
                        ok: false,
                        output: None,
                        error: Some(daemon_protocol::ActionError {
                            code: "consent_denied".into(),
                            message: "El usuario denegó el consentimiento para esta acción.".into(),
                        }),
                        duration_ms: 0,
                    };
                    ws.send(Message::Text(serde_json::to_string(
                        &BridgeFrame::ActionResult(result),
                    )?))
                    .await?;
                    return Ok(());
                }
                // Approved. If the user chose "always allow", persist the
                // path into the gate snapshot so future actions on it skip
                // the prompt.
                if answer.remember {
                    if let Some(path) = target {
                        let mut g = self.gate.lock().await;
                        let augmented = g.with_additional_path(std::path::PathBuf::from(path));
                        *g = augmented;
                    }
                }
            }
        }

        // 4. REAL execution. The previous scaffold ACKed with ok:true
        //    without doing anything — the web agent reported success
        //    while the user's file was never touched. That lie is the
        //    entire bug this module now refuses to reproduce.
        let started = std::time::Instant::now();
        match req.capability.as_str() {
            "desktop.fs.read" => {
                let parse: Result<crate::fs_ops::FsReadRequest> =
                    serde_json::from_value(req.params.clone()).map_err(DaemonError::Json);
                match parse {
                    Ok(fs_req) => {
                        // If the path is outside alwaysAllow, prompt once
                        // for consent before touching the file. Approval
                        // augments the gate for THIS operation only (unless
                        // the user checked "remember", which persists).
                        let gate_decision = {
                            let g = self.gate.lock().await;
                            g.check_path("desktop.fs.read", &fs_req.path)
                        };
                        let effective_gate = match gate_decision {
                            GateDecision::Allow => self.gate.lock().await.clone(),
                            GateDecision::Deny => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some("consent_denied".into()),
                                    0,
                                )
                                .await?;
                                return Ok(());
                            }
                            GateDecision::RequireConsent => {
                                if !self.await_path_consent(&req, &fs_req.path).await {
                                    self.send_action_result(
                                        ws,
                                        req.id,
                                        false,
                                        None,
                                        Some("consent_denied".into()),
                                        0,
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                self.gate
                                    .lock()
                                    .await
                                    .with_additional_path(fs_req.path.clone())
                            }
                        };
                        let ops = crate::fs_ops::FsOps::new(&effective_gate);
                        match ops.read(fs_req).await {
                            Ok(res) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    true,
                                    Some(serde_json::json!({
                                        "content_base64": res.content_base64,
                                        "size": res.size,
                                    })),
                                    None,
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                            Err(e) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some(e.to_string()),
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        self.send_action_result(
                            ws,
                            req.id,
                            false,
                            None,
                            Some(format!("bad params: {e}")),
                            0,
                        )
                        .await?;
                    }
                }
            }
            "desktop.fs.write" => {
                let parse: Result<crate::fs_ops::FsWriteRequest> =
                    serde_json::from_value(req.params.clone()).map_err(DaemonError::Json);
                match parse {
                    Ok(fs_req) => {
                        let gate_decision = {
                            let g = self.gate.lock().await;
                            g.check_path("desktop.fs.write", &fs_req.path)
                        };
                        let effective_gate = match gate_decision {
                            GateDecision::Allow => self.gate.lock().await.clone(),
                            GateDecision::Deny => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some("consent_denied".into()),
                                    0,
                                )
                                .await?;
                                return Ok(());
                            }
                            GateDecision::RequireConsent => {
                                if !self.await_path_consent(&req, &fs_req.path).await {
                                    self.send_action_result(
                                        ws,
                                        req.id,
                                        false,
                                        None,
                                        Some("consent_denied".into()),
                                        0,
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                self.gate
                                    .lock()
                                    .await
                                    .with_additional_path(fs_req.path.clone())
                            }
                        };
                        let ops = crate::fs_ops::FsOps::new(&effective_gate);
                        match ops.write(fs_req).await {
                            Ok(res) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    res.verified,
                                    Some(serde_json::json!({
                                        "bytes_written": res.bytes_written,
                                        "verified": res.verified,
                                    })),
                                    if res.verified {
                                        None
                                    } else {
                                        Some("write completed but read-back verification FAILED — file content mismatch".into())
                                    },
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                            Err(e) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some(e.to_string()),
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        self.send_action_result(
                            ws,
                            req.id,
                            false,
                            None,
                            Some(format!("bad params: {e}")),
                            0,
                        )
                        .await?;
                    }
                }
            }
            "desktop.fs.verify" => {
                // Verification is how the web UI decides whether to attach
                // a workspace — the path is NOT yet in alwaysAllowPaths, so
                // it must NOT go through gate_for_path (that would always
                // reject). The capability grant alone is the gate here; the
                // probe touches nothing but a unique temp file.
                let parse: Result<crate::fs_ops::FsVerifyRequest> =
                    serde_json::from_value(req.params.clone()).map_err(DaemonError::Json);
                match parse {
                    Ok(fs_req) => {
                        let g = self.gate.lock().await;
                        let ops = crate::fs_ops::FsOps::new(&g);
                        let res = ops.verify(fs_req).await;
                        drop(g);
                        self.send_action_result(
                            ws,
                            req.id,
                            res.exists && res.readable && res.writable,
                            Some(serde_json::json!({
                                "exists": res.exists,
                                "is_dir": res.is_dir,
                                "readable": res.readable,
                                "writable": res.writable,
                            })),
                            res.error,
                            started.elapsed().as_millis() as u64,
                        )
                        .await?;
                    }
                    Err(e) => {
                        self.send_action_result(
                            ws,
                            req.id,
                            false,
                            None,
                            Some(format!("bad params: {e}")),
                            0,
                        )
                        .await?;
                    }
                }
            }
            "desktop.fs.delete" => {
                let parse: Result<crate::fs_ops::FsDeleteRequest> =
                    serde_json::from_value(req.params.clone()).map_err(DaemonError::Json);
                match parse {
                    Ok(fs_req) => {
                        let gate_decision = {
                            let g = self.gate.lock().await;
                            g.check_path("desktop.fs.delete", &fs_req.path)
                        };
                        let effective_gate = match gate_decision {
                            GateDecision::Allow => self.gate.lock().await.clone(),
                            GateDecision::Deny => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some("consent_denied".into()),
                                    0,
                                )
                                .await?;
                                return Ok(());
                            }
                            GateDecision::RequireConsent => {
                                if !self.await_path_consent(&req, &fs_req.path).await {
                                    self.send_action_result(
                                        ws,
                                        req.id,
                                        false,
                                        None,
                                        Some("consent_denied".into()),
                                        0,
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                self.gate
                                    .lock()
                                    .await
                                    .with_additional_path(fs_req.path.clone())
                            }
                        };
                        let ops = crate::fs_ops::FsOps::new(&effective_gate);
                        match ops.delete(fs_req).await {
                            Ok(()) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    true,
                                    Some(serde_json::json!({ "deleted": true })),
                                    None,
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                            Err(e) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some(e.to_string()),
                                    started.elapsed().as_millis() as u64,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        self.send_action_result(
                            ws,
                            req.id,
                            false,
                            None,
                            Some(format!("bad params: {e}")),
                            0,
                        )
                        .await?;
                    }
                }
            }
            "desktop.shell.execute" => {
                let parse: Result<crate::shell::ShellRequest> =
                    serde_json::from_value(req.params.clone()).map_err(DaemonError::Json);
                match parse {
                    Ok(shell_req) => {
                        // Isolated scope: the runner borrows the gate guard;
                        // both must be dropped before the streaming loop.
                        let run_result = {
                            let g = self.gate.lock().await;
                            let runner = crate::shell::ShellRunner::new(&g);
                            runner.run(shell_req).await
                        };
                        match run_result {
                            Ok((mut rx, future)) => {
                                // The future owns the channel's last `tx`
                                // (dropped inside await_result). Run it
                                // concurrently so the drain loop below can
                                // terminate — awaiting it after the loop
                                // would deadlock (recv() never returns None
                                // while the future holds a sender).
                                let handle =
                                    tokio::spawn(async move { future.await_result().await });
                                let mut seq = 0u64;
                                let action_id_stream = req.id.clone();
                                while let Some(chunk) = rx.recv().await {
                                    let frame = daemon_protocol::ActionStreamFrame {
                                        v: PROTOCOL_VERSION,
                                        id: action_id_stream.clone(),
                                        seq,
                                        channel: match chunk.channel {
                                            "stdout" => daemon_protocol::StreamChannel::Stdout,
                                            _ => daemon_protocol::StreamChannel::Stderr,
                                        },
                                        data: chunk.data,
                                        eof: false,
                                    };
                                    ws.send(Message::Text(serde_json::to_string(
                                        &BridgeFrame::ActionStream(frame),
                                    )?))
                                    .await?;
                                    seq += 1;
                                }
                                match handle.await {
                                    Ok(Ok(res)) => {
                                        let ok = res.exit_code == Some(0);
                                        self.send_action_result(
                                            ws,
                                            req.id,
                                            ok,
                                            Some(serde_json::json!({
                                                "exit_code": res.exit_code,
                                                "stdout": res.stdout,
                                                "stderr": res.stderr,
                                            })),
                                            if ok {
                                                None
                                            } else {
                                                Some(format!("exit code {:?}", res.exit_code))
                                            },
                                            res.duration_ms,
                                        )
                                        .await?;
                                    }
                                    Ok(Err(e)) => {
                                        self.send_action_result(
                                            ws,
                                            req.id,
                                            false,
                                            None,
                                            Some(e.to_string()),
                                            started.elapsed().as_millis() as u64,
                                        )
                                        .await?;
                                    }
                                    Err(e) => {
                                        self.send_action_result(
                                            ws,
                                            req.id,
                                            false,
                                            None,
                                            Some(format!("shell task failed: {e}")),
                                            started.elapsed().as_millis() as u64,
                                        )
                                        .await?;
                                    }
                                }
                            }
                            Err(e) => {
                                self.send_action_result(
                                    ws,
                                    req.id,
                                    false,
                                    None,
                                    Some(e.to_string()),
                                    0,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        self.send_action_result(
                            ws,
                            req.id,
                            false,
                            None,
                            Some(format!("bad params: {e}")),
                            0,
                        )
                        .await?;
                    }
                }
            }
            other => {
                self.send_action_result(
                    ws,
                    req.id,
                    false,
                    None,
                    Some(format!("Capability not implemented in daemon: {other}")),
                    0,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Ask the user for consent to touch a path outside alwaysAllow.
    /// Returns true on approval (and persists the path when "remember"
    /// was checked), false on denial/timeout.
    async fn await_path_consent(
        &self,
        req: &daemon_protocol::ActionRequestFrame,
        path: &std::path::Path,
    ) -> bool {
        let prompt = ConsentPrompt {
            action_id: req.id.clone(),
            capability: req.capability.clone(),
            summary: format!(
                "{} en {}",
                req.capability,
                path.display()
            ),
            path: Some(path.display().to_string()),
        };
        let mut rx = self.consent.ask(prompt);
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            &mut rx,
        )
        .await;
        let answer: ConsentAnswer = match answer {
            Ok(Ok(a)) => a,
            Ok(Err(_)) | Err(_) => ConsentAnswer::default(),
        };
        if answer.approved && answer.remember {
            let mut g = self.gate.lock().await;
            *g = g.with_additional_path(path.to_path_buf());
        }
        answer.approved
    }

    async fn send_action_result(
        &self,
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        action_id: String,
        ok: bool,
        output: Option<serde_json::Value>,
        error: Option<String>,
        duration_ms: u64,
    ) -> Result<()> {
        let result = daemon_protocol::ActionResultFrame {
            v: PROTOCOL_VERSION,
            id: action_id,
            ok,
            output,
            error: error.map(|e| daemon_protocol::ActionError {
                code: "action_failed".into(),
                message: e,
            }),
            duration_ms,
        };
        ws.send(Message::Text(serde_json::to_string(
            &BridgeFrame::ActionResult(result),
        )?))
        .await?;
        Ok(())
    }

    /// `sync.chat.push` — the server sends conversation snapshots so
    /// the desktop daemon keeps a durable local archive (SQLite in
    /// the user's config dir). This is the only chat-sync capability
    /// in this version; reads/export happen in the daemon UI only.
    async fn handle_chat_push(
        &self,
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        req: daemon_protocol::ActionRequestFrame,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let (ok, error) = match parse_chat_push_params(&req.params) {
            Ok(convs) => {
                let mut saved = 0usize;
                for conv in &convs {
                    match self.chat_store.upsert_conversation(conv) {
                        Ok(n) => saved += n,
                        Err(e) => {
                            tracing::error!("[chat-sync] upsert {} failed: {e}", conv.id);
                            return self
                                .send_chat_push_result(
                                    ws,
                                    req.id,
                                    false,
                                    format!("store_error: {e}"),
                                    started.elapsed().as_millis() as u64,
                                )
                                .await;
                        }
                    }
                }
                tracing::info!(
                    "[chat-sync] pushed {} conversations, {} messages saved",
                    convs.len(),
                    saved
                );
                (true, None)
            }
            Err(e) => {
                tracing::warn!("[chat-sync] malformed push: {e}");
                (false, Some(format!("bad_params: {e}")))
            }
        };

        self.send_chat_push_result(
            ws,
            req.id,
            ok,
            error.unwrap_or_default(),
            started.elapsed().as_millis() as u64,
        )
        .await
    }

    async fn send_chat_push_result(
        &self,
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        action_id: String,
        ok: bool,
        error: String,
        duration_ms: u64,
    ) -> Result<()> {
        let result = daemon_protocol::ActionResultFrame {
            v: PROTOCOL_VERSION,
            id: action_id,
            ok,
            output: None,
            error: if error.is_empty() {
                None
            } else {
                Some(daemon_protocol::ActionError {
                    code: "chat_sync_failed".into(),
                    message: error,
                })
            },
            duration_ms,
        };
        ws.send(Message::Text(serde_json::to_string(
            &BridgeFrame::ActionResult(result),
        )?))
        .await?;
        Ok(())
    }
}
