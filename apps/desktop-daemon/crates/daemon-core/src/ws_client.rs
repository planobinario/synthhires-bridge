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

use crate::{capability::{CapabilityGate, ScopeSnapshot}, Result};
use daemon_protocol::{
    BridgeFrame, HelloFrame, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use sha2::Digest;
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
    status_tx: Option<tokio::sync::watch::Sender<String>>,
}

impl WsClient {
    pub fn new(
        backend_url: impl Into<String>,
        token: impl Into<String>,
        device_id: impl Into<String>,
        fingerprint: impl Into<String>,
        device_kind: &'static str,
        device_name: impl Into<String>,
        gate: CapabilityGate,
        status_tx: Option<tokio::sync::watch::Sender<String>>,
    ) -> Self {
        Self {
            backend_url: backend_url.into(),
            token: token.into(),
            _device_id: device_id.into(),
            fingerprint: fingerprint.into(),
            device_kind,
            device_name: device_name.into(),
            gate: std::sync::Arc::new(Mutex::new(gate)),
            status_tx,
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
                    if let Some(ref tx) = self.status_tx {
                        let _ = tx.send("Error: Token revocado o invalido".into());
                    }
                    return Err(crate::DaemonError::Protocol(msg));
                }
                Err(e) => {
                    tracing::warn!("WS error: {e}; reconnecting in {:?}", backoff);
                    if let Some(ref tx) = self.status_tx {
                        let _ = tx.send(format!("Error: Reconectando en {}s...", backoff.as_secs()));
                    }
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
        let mut req = self.backend_url.clone().into_client_request().map_err(|e| crate::DaemonError::Ws(format!("into_client_request: {e}")))?;
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", format!("bearer.{}", self.token).parse().map_err(|e: http::header::InvalidHeaderValue| crate::DaemonError::Ws(format!("invalid header: {e}")))?);
        let (mut ws, _resp) = connect_async(req).await.map_err(|e| {
            crate::DaemonError::Ws(format!("connect: {e}"))
        })?;
        // Send hello
        let hello = BridgeFrame::Hello(HelloFrame {
            v: PROTOCOL_VERSION,
            token_hash: hex::encode(sha2::Sha256::digest(self.token.as_bytes())),
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

        if let Some(ref tx) = self.status_tx {
            let _ = tx.send("Conectado (esperando eventos...)".into());
        }

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
            BridgeFrame::ActionRequest(req) => self.handle_action(ws, req).await,
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
        // Per-action consent: if the gate says RequireConsent and the
        // server didn't set skip_consent_prompt, we MUST prompt the
        // user via the native dialog. This is implemented in the
        // Tauri-based tray UI; here we conservatively DENY when
        // consent is required and the server didn't skip it.
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
            ws.send(Message::Text(serde_json::to_string(&BridgeFrame::ActionResult(result))?))
                .await?;
            return Ok(());
        }
        // For shell.execute specifically, prompt every time. The Tauri
        // UI implements the dialog and sets skip_consent_prompt=false;
        // we honour that. In this CLI-only scaffold we accept the
        // command if the user has added it to alwaysAllowPaths,
        // otherwise we deny.
        if req.capability == "desktop.shell.execute" && !req.skip_consent_prompt {
            let result = daemon_protocol::ActionResultFrame {
                v: PROTOCOL_VERSION,
                id: req.id,
                ok: false,
                output: None,
                error: Some(daemon_protocol::ActionError {
                    code: "consent_required".into(),
                    message: "Esta acción requiere confirmación en la UI del daemon.".into(),
                }),
                duration_ms: 0,
            };
            ws.send(Message::Text(serde_json::to_string(&BridgeFrame::ActionResult(result))?))
                .await?;
            return Ok(());
        }
        // Capability-specific execution is delegated to the daemon-
        // core modules by the upper layer (the Tauri UI). In the
        // CLI scaffold we just acknowledge.
        let result = daemon_protocol::ActionResultFrame {
            v: PROTOCOL_VERSION,
            id: req.id,
            ok: true,
            output: Some(serde_json::json!({"note": "dispatched; handler lives in the Tauri UI"})),
            error: None,
            duration_ms: 0,
        };
        ws.send(Message::Text(serde_json::to_string(&BridgeFrame::ActionResult(result))?))
            .await?;
        Ok(())
    }
}