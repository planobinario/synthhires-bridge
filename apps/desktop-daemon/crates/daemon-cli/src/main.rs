//! SynthHires Desktop Daemon — binary entry point.
//!
//! Lifecycle:
//!   1. Parse CLI args (clap).
//!   2. Acquire single-instance lock (if already running, open local dashboard in browser).
//!   3. Load state from ~/.config/synthhires-bridge/state.json.
//!   4. If paired, read token from OS keyring, spawn WS client loop.
//!   5. Spawn local axum HTTP server on 127.0.0.1:7333 with CORS and HTML status dashboard.
//!   6. Open local dashboard in default browser on launch.
//!   7. Spawn system tray icon with menu (Status, Open Dashboard, Unpair, Quit).
//!   8. Block until Quit is selected from the tray menu.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use clap::Parser;
use daemon_core::{
    capability::{CapabilityGate, ScopeSnapshot},
    fingerprint::DeviceFingerprint,
    keyring::TokenStore,
    ws_client::WsClient,
    DaemonError, Result,
};

mod tray;
mod ui;

#[derive(Parser, Debug)]
#[command(
    name = "synthhires-bridge",
    version,
    about = "Bridges your agents to your PC's filesystem and terminal."
)]
struct Cli {
    #[arg(long, env = "SYNTHHIRES_BACKEND_URL")]
    backend_url: Option<String>,

    #[arg(long, env = "SYNTHHIRES_LOCAL_PORT")]
    local_port: Option<u16>,

    #[arg(long)]
    config_dir: Option<PathBuf>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct DaemonState {
    device_id: Option<String>,
    scopes: daemon_protocol::Scopes,
}

impl DaemonState {
    async fn load(config_dir: &std::path::Path) -> Result<Self> {
        let path = config_dir.join("state.json");
        if !path.exists() {
            return Ok(Self {
                device_id: None,
                scopes: daemon_protocol::Scopes::default(),
            });
        }
        let raw = tokio::fs::read(&path).await.map_err(DaemonError::Io)?;
        Ok(serde_json::from_slice(&raw).unwrap_or(Self {
            device_id: None,
            scopes: daemon_protocol::Scopes::default(),
        }))
    }

    async fn save(&self, config_dir: &std::path::Path) -> Result<()> {
        let path = config_dir.join("state.json");
        let raw = serde_json::to_vec_pretty(self).unwrap();
        tokio::fs::write(&path, raw).await.map_err(DaemonError::Io)?;
        Ok(())
    }
}

fn main() -> Result<()> {
    // Start tracing early
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,daemon_core=debug")
                }),
        )
        .init();

    // Spawn Tokio in a separate background thread so it doesn't block the UI
    let (status_tx, status_rx) = tokio::sync::watch::channel("Iniciando...".to_string());
    let (tasks_tx, tasks_rx) = tokio::sync::watch::channel(Vec::new());
    let (kill_tx, kill_rx) = tokio::sync::mpsc::channel(100);
    
    let ui_ctx = Arc::new(tokio::sync::RwLock::new(None));
    let ui_ctx_clone = ui_ctx.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime");
            
        rt.block_on(async move {
            if let Err(e) = background_daemon_task(status_tx, tasks_tx, kill_rx, ui_ctx_clone).await {
                tracing::error!("Daemon fatal error: {e}");
            }
        });
    });

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([400.0, 500.0])
            .with_title("SynthHires Bridge"),
        ..Default::default()
    };
    
    eframe::run_native(
        "synthhires-bridge",
        native_options,
        Box::new(move |cc| {
            let app = ui::BridgeApp::new(cc, status_rx, tasks_rx, kill_tx);
            let mut w_ctx = ui_ctx.blocking_write();
            *w_ctx = Some(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    ).map_err(|e| DaemonError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

    Ok(())
}

async fn background_daemon_task(
    status_tx: tokio::sync::watch::Sender<String>,
    tasks_tx: tokio::sync::watch::Sender<Vec<daemon_core::task_registry::TaskState>>,
    mut kill_rx: tokio::sync::mpsc::Receiver<uuid::Uuid>,
    ui_ctx: Arc<tokio::sync::RwLock<Option<eframe::egui::Context>>>,
) -> Result<()> {

    let cli = Cli::parse();

    let config_dir = cli.config_dir.clone().unwrap_or_else(|| {
        directories::ProjectDirs::from("com", "synthhires", "bridge")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| {
                dirs_next::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("synthhires-bridge")
            })
    });
    std::fs::create_dir_all(&config_dir).ok();

    let local_port = cli.local_port.unwrap_or(7333);

    let web_url = std::env::var("SYNTHHIRES_WEB_URL")
        .unwrap_or_else(|_| "http://localhost:4321/space/runtimes".to_string());

    let lock = single_instance::SingleInstance::new("synthhires-bridge");
    let deep_link = std::env::args().find(|a| a.starts_with("synthhires://"));

    if let Ok(ref instance) = lock {
        if !instance.is_single() {
            tracing::info!("Bridge is already running.");
            // Send deep link via IPC to the running instance
            if let Some(link) = deep_link {
                use interprocess::local_socket::prelude::*;
                use std::io::Write;
                
                let name = if cfg!(windows) {
                    "synthhires-bridge-ipc".to_ns_name::<interprocess::local_socket::GenericNamespaced>().unwrap()
                } else {
                    "/tmp/synthhires-bridge-ipc.sock".to_fs_name::<interprocess::local_socket::GenericFilePath>().unwrap()
                };
                
                if let Ok(mut conn) = interprocess::local_socket::Stream::connect(name) {
                    let _ = conn.write_all(link.as_bytes());
                    // Wait for ACK
                    let mut buf = [0u8; 3];
                    use std::io::Read;
                    let _ = conn.read_exact(&mut buf);
                }
            } else {
                // Just let the existing instance's native UI handle it, 
                // or we could send a command to focus the window.
                tracing::info!("No deep link provided. Existing instance is already running.");
            }
            return Ok(());
        }
    } else {
        tracing::error!("Could not acquire single instance lock");
        return Ok(());
    }

    let state = Arc::new(RwLock::new(DaemonState::load(&config_dir).await?));

    let backend_url = cli
        .backend_url
        .unwrap_or_else(|| "wss://app.synthhires.com/api/devices/ws".to_string());

    // Determine pairing status
    let is_paired = {
        let s = state.read().await;
        s.device_id.is_some()
    };

    use daemon_core::task_registry::TaskRegistry;
    let task_registry = Arc::new(tokio::sync::RwLock::new(TaskRegistry::new(200)));

    // Task Registry loop
    tokio::spawn({
        let tr = task_registry.clone();
        let t_tx = tasks_tx.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                interval.tick().await;
                let mut reg = tr.write().await;
                reg.cleanup_stale_tasks(std::time::Duration::from_secs(5));
                // Extract states for UI
                let states: Vec<_> = reg.states().cloned().collect();
                let _ = t_tx.send(states);
            }
        }
    });

    // Kill Task loop
    tokio::spawn({
        let tr = task_registry.clone();
        async move {
            while let Some(id) = kill_rx.recv().await {
                let mut reg = tr.write().await;
                reg.kill_task(id).await;
            }
        }
    });

    // IPC Listener for deep links
    tokio::spawn({
        let config_dir_ipc = config_dir.clone();
        let backend_url_ipc = backend_url.clone();
        let state_ipc = state.clone();
        
        async move {
            use interprocess::local_socket::prelude::*;
            use interprocess::local_socket::ListenerOptions;
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            use interprocess::local_socket::traits::tokio::Listener;

            let name = if cfg!(windows) {
                "synthhires-bridge-ipc".to_ns_name::<interprocess::local_socket::GenericNamespaced>().unwrap()
            } else {
                "/tmp/synthhires-bridge-ipc.sock".to_fs_name::<interprocess::local_socket::GenericFilePath>().unwrap()
            };
            
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file("/tmp/synthhires-bridge-ipc.sock");
            }

            let mut options = ListenerOptions::new().name(name);

            #[cfg(windows)]
            {
                use interprocess::os::windows::local_socket::ListenerOptionsExt;
                use std::os::windows::ffi::OsStrExt;
                use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
                use std::ptr;
                use interprocess::os::windows::security_descriptor::AsSecurityDescriptorExt;
                
                let sddl: Vec<u16> = std::ffi::OsStr::new("D:(A;;GA;;;OW)").encode_wide().chain(std::iter::once(0)).collect();
                let mut sd: *mut std::ffi::c_void = ptr::null_mut();
                
                unsafe {
                    if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        1, // SDDL_REVISION_1
                        &mut sd,
                        ptr::null_mut(),
                    ) != 0 {
                        use interprocess::os::windows::security_descriptor::BorrowedSecurityDescriptor;
                        let bsd = BorrowedSecurityDescriptor::from_ptr(sd);
                        if let Ok(owned_sd) = bsd.to_owned_sd() {
                            options = options.security_descriptor(owned_sd);
                        }
                    } else {
                        tracing::error!("Failed to set Windows ACL on Named Pipe, error: {}", std::io::Error::last_os_error());
                    }
                }
            }

            #[cfg(unix)]
            {
                use interprocess::os::unix::local_socket::ListenerOptionsExt;
                options = options.mode(0o600);
            }

            if let Ok(listener) = options.create_tokio() {
                tracing::info!("IPC Listener bound successfully");
                loop {
                    if let Ok(mut stream) = listener.accept().await {
                        let state_clone = state_ipc.clone();
                        let backend_url_clone = backend_url_ipc.clone();
                        let config_dir_clone = config_dir_ipc.clone();
                        
                        tokio::spawn(async move {
                            let mut buf = [0u8; 1024];
                            if let Ok(len) = stream.read(&mut buf).await {
                                let msg = String::from_utf8_lossy(&buf[..len]).to_string();
                                tracing::info!("IPC Received data length: {}", len);
                                
                                if let Some(uri) = msg.strip_prefix("synthhires://") {
                                    let clean_uri = uri.trim_end_matches('/').trim();
                                    
                                    // Handle `pair?token=...` or `...` directly
                                    let clean_token = if let Some(t) = clean_uri.strip_prefix("pair?token=") {
                                        t
                                    } else {
                                        clean_uri
                                    };
                                    
                                    tracing::info!("Processing deep link token: {}", clean_token);
                                    if let Ok(_) = daemon_core::keyring::TokenStore::save(clean_token, clean_token) {
                                        let mut s = state_clone.write().await;
                                        s.device_id = Some(clean_token.to_string());
                                        let _ = s.save(&config_dir_clone).await;
                                        
                                        // Start WS client
                                        let ws_state = state_clone.clone();
                                        let ws_backend = backend_url_clone;
                                        tokio::spawn(async move {
                                            if let Err(e) = run_ws_client(ws_state, ws_backend).await {
                                                tracing::error!("WS client died: {e}");
                                            }
                                        });

                                        tracing::info!("Token saved and WS client started. Sending ACK.");
                                        let _ = stream.write_all(b"ACK").await;
                                    } else {
                                        tracing::error!("Failed to save token to keyring");
                                    }
                                } else {
                                    tracing::warn!("Received unrecognized IPC message");
                                }
                            }
                        });
                    }
                }
            } else {
                tracing::error!("Failed to bind IPC listener");
            }
        }
    });

    // We no longer auto-open the web app on launch,
    // we rely on the native egui window to show status.

    // Spawn WS client if already paired
    if is_paired {
        let ws_state = state.clone();
        let ws_backend = backend_url.clone();
        tokio::spawn(async move {
            if let Err(e) = run_ws_client(ws_state, ws_backend).await {
                tracing::error!("WS client died: {e}");
            }
        });
    }

    let (_tray_handle, quit_rx) = tray::build_tray(state.clone(), config_dir.clone(), local_port, ui_ctx.clone())?;
    tracing::info!("Daemon background tasks running.");
    
    // Update status to let UI know
    let _ = status_tx.send("Conectado (esperando eventos...)".to_string());

    // Block the tokio thread until quit signal
    _ = quit_rx.await;
    tracing::info!("tokio background daemon exiting");
    Ok(())
}

async fn run_ws_client(state: Arc<RwLock<DaemonState>>, backend_url: String) -> Result<()> {
    let device_id = {
        let s = state.read().await;
        s.device_id
            .clone()
            .ok_or_else(|| DaemonError::Keyring("not paired".into()))?
    };

    let token = TokenStore::load(&device_id)
        .map_err(|e| DaemonError::Keyring(format!("token load: {e}")))?
        .ok_or_else(|| DaemonError::Keyring("token not found in keyring".into()))?;

    let fp = DeviceFingerprint::collect().hash_hex();

    let gate = CapabilityGate::new(ScopeSnapshot::default());
    let ws = WsClient::new(
        backend_url,
        token,
        device_id,
        fp,
        "desktop",
        hostname(),
        gate,
    );
    ws.run().await
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

