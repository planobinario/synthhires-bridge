//! SynthHires Desktop Daemon — binary entry point.
//!
//! Lifecycle:
//!   1. Parse CLI args (clap).
//!   2. Acquire single-instance lock (prevents two daemons fighting
//!      over the same keyring slot on the same machine).
//!   3. Read ~/.config/synthhires/config.toml for the pairing state
//!      (the deviceId + the cached scopes). If no paired deviceId is
//!      present, drop to "pairing mode" — print instructions and the
//!      local UI URL.
//!   4. Spawn the tray icon (system_tray crate).
//!   5. Spawn the local axum HTTP server on 127.0.0.1:7333 (status
//!      page, "revoke this device" button).
//!   6. Spawn the WS client, which dials the backend's
//!      /api/devices/ws endpoint with the token from the keyring.
//!   7. The WS client loop handles hello, scope_update, action_request,
//!      revoke, and reconnect-with-backoff.

use clap::{Parser, Subcommand};
use daemon_core::{
    audit::{AuditEntry, AuditLog},
    capability::{CapabilityGate, ScopeSnapshot},
    fingerprint::DeviceFingerprint,
    keyring::TokenStore,
    shell::{ShellRequest, ShellRunner},
    ws_client::WsClient,
    Result,
};
use daemon_protocol::Scopes;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Parser, Debug)]
#[command(
    name = "synthhires-bridge",
    version,
    about = "Bridges your agents to your PC's filesystem and terminal."
)]
struct Cli {
    /// Override the backend WebSocket URL (default: wss://app.synthhires.com/api/devices/ws).
    #[arg(long, env = "SYNTHHIRES_BACKEND_URL")]
    backend_url: Option<String>,
    /// Override the local UI port (default: 7333).
    #[arg(long, env = "SYNTHHIRES_LOCAL_PORT")]
    local_port: Option<u16>,
    /// Path to the config directory (default: OS-specific via `directories`).
    #[arg(long)]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print pairing instructions + start a tiny webserver to receive
    /// the deviceId once the user enters the code in the desktop UI.
    Pair,
    /// Print daemon + pairing status.
    Status,
    /// Forget the current pairing (revoke locally + clear keyring).
    Unpair,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,daemon_core=debug")),
        )
        .json()
        .init();

    let cli = Cli::parse();

    let config_dir = cli
        .config_dir
        .clone()
        .or_else(|| directories::ProjectDirs::from("com", "synthhires", "bridge").map(|d| d.config_dir().to_path_buf()))
        .ok_or_else(|| daemon_core::DaemonError::Keyring("no config dir".into()))?;
    std::fs::create_dir_all(&config_dir).ok();

    // Single-instance: prevents two daemons from racing over the
    // same keyring slot and double-firing tray notifications.
    let _lock = single_instance::SingleInstance::new("synthhires-bridge")
        .map_err(|e| daemon_core::DaemonError::Keyring(format!("already running: {e}")))?;

    let state = Arc::new(RwLock::new(DaemonState::load(&config_dir).await?));

    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Pair => pair_flow(state.clone(), &config_dir).await,
        Cmd::Status => status_flow(state.clone()).await,
        Cmd::Unpair => unpair_flow(state.clone(), &config_dir).await,
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct DaemonState {
    /// The deviceId we got from the server at /pair/complete.
    device_id: Option<String>,
    scopes: Scopes,
}

impl DaemonState {
    async fn load(config_dir: &std::path::Path) -> Result<Self> {
        let path = config_dir.join("state.json");
        if !path.exists() {
            return Ok(Self { device_id: None, scopes: Scopes::default() });
        }
        let raw = tokio::fs::read(&path).await.map_err(daemon_core::DaemonError::Io)?;
        Ok(serde_json::from_slice(&raw).unwrap_or(Self {
            device_id: None,
            scopes: Scopes::default(),
        }))
    }
    async fn save(&self, config_dir: &std::path::Path) -> Result<()> {
        let path = config_dir.join("state.json");
        let raw = serde_json::to_vec_pretty(self).unwrap();
        tokio::fs::write(&path, raw).await.map_err(daemon_core::DaemonError::Io)?;
        Ok(())
    }
}

async fn status_flow(state: Arc<RwLock<DaemonState>>) -> Result<()> {
    let s = state.read().await;
    println!("SynthHires Bridge");
    println!("  paired:       {}", s.device_id.is_some());
    if let Some(id) = &s.device_id {
        println!("  device_id:    {id}");
    }
    println!("  capabilities: {:?}", s.scopes.capabilities);
    println!("  always_allow: {:?}", s.scopes.always_allow_paths);
    Ok(())
}

async fn unpair_flow(state: Arc<RwLock<DaemonState>>, config_dir: &std::path::Path) -> Result<()> {
    let id = state.read().await.device_id.clone();
    if let Some(id) = id {
        TokenStore::delete(&id)?;
    }
    let mut s = state.write().await;
    s.device_id = None;
    s.scopes = Scopes::default();
    s.save(config_dir).await?;
    println!("Unpaired. The device can be revoked from /space/connections in the web UI.");
    Ok(())
}

async fn pair_flow(state: Arc<RwLock<DaemonState>>, config_dir: &std::path::Path) -> Result<()> {
    // Print instructions + start a localhost UI that accepts the
    // deviceId+token returned by the server. The user enters the 6-char
    // code in the web UI (/space/connections → "Emparejar nuevo
    // dispositivo") and the server returns the deviceId+raw token via
    // a local listener on this machine.
    println!("Pairing mode");
    println!("1. Open https://app.synthhires.com/space/connections?tab=devices");
    println!("2. Click 'Emparejar nuevo dispositivo'");
    println!("3. Select 'Desktop' and copy the 6-character code");
    println!("4. Paste the code into the dialog that opened in your browser");
    println!();
    println!("Listening on http://127.0.0.1:7333/pair for the deviceId+token...");
    let app = axum::Router::new().route(
        "/pair",
        axum::routing::post({
            let state = state.clone();
            let config_dir = config_dir.to_path_buf();
            move |axum::Json(payload): axum::Json<PairPayload>| async move {
                let mut s = state.write().await;
                s.device_id = Some(payload.device_id.clone());
                s.scopes = payload.scopes.clone();
                if let Err(e) = s.save(&config_dir).await {
                    return Err((axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("save failed: {e}")));
                }
                if let Err(e) = TokenStore::save(&payload.device_id, &payload.token) {
                    return Err((axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("token save failed: {e}")));
                }
                Ok(axum::Json(serde_json::json!({"ok": true})))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:7333").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct PairPayload {
    device_id: String,
    token: String,
    scopes: Scopes,
}

// Suppress unused-import warnings for items kept around for the
// upcoming connect-loop implementation.
#[allow(dead_code)]
fn _unused(_: &AuditLog, _: &ShellRunner, _: &WsClient, _: &AuditEntry, _: &ShellRequest) {
    let _ = CapabilityGate::new(ScopeSnapshot::default());
    let _ = DeviceFingerprint::collect();
}