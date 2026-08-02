#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
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
    backend_url: Option<String>,
    scopes: daemon_protocol::Scopes,
}

impl DaemonState {
    async fn load(config_dir: &std::path::Path) -> Result<Self> {
        let path = config_dir.join("state.json");
        if !path.exists() {
            return Ok(Self {
                device_id: None,
                backend_url: None,
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,daemon_core=debug")),
        )
        .init();

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

    // Check single instance synchronously
    let lock = single_instance::SingleInstance::new("synthhires-bridge");
    let deep_link = std::env::args().find(|a| a.starts_with("synthhires://"));
    let mut is_already_running = false;
    if let Ok(ref instance) = lock {
        if !instance.is_single() {
            tracing::info!("Bridge is already running.");
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
                    let mut buf = [0u8; 3];
                    use std::io::Read;
                    let _ = conn.read_exact(&mut buf);
                }
            } else {
                tracing::info!("No deep link provided. Existing instance is already running.");
            }
            is_already_running = true;
        }
    }

    let local_port = cli.local_port.unwrap_or(7333);
    
    // Load state synchronously for main thread
    let rt_temp = tokio::runtime::Runtime::new().unwrap();
    let initial_state = rt_temp.block_on(async {
        DaemonState::load(&config_dir).await.unwrap_or_else(|_| DaemonState {
            device_id: None,
            scopes: daemon_protocol::Scopes::default(),
        })
    });
    drop(rt_temp);
    
    let state = Arc::new(RwLock::new(initial_state));
    let ui_ctx = Arc::new(tokio::sync::RwLock::new(None));

    let (status_tx, status_rx) = tokio::sync::watch::channel("Iniciando...".to_string());
    let (tasks_tx, tasks_rx) = tokio::sync::watch::channel(Vec::new());
    let (kill_tx, kill_rx) = tokio::sync::mpsc::channel(100);

    // Build tray ON THE MAIN THREAD to avoid COM initialization E_FAIL errors on Windows
    // We will do it inside the eframe closure where COM is already initialized!
    
    let backend_url = cli
        .backend_url
        .unwrap_or_else(|| "wss://app.synthhires.com/api/devices/ws".to_string());

    let state_clone = state.clone();
    let config_dir_clone = config_dir.clone();
    let (quit_tx, quit_rx) = tokio::sync::oneshot::channel();
    
    // Spawn background tasks only if we are the primary instance
    if !is_already_running {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");
                
            rt.block_on(async move {
                background_daemon_task(
                    state_clone,
                    config_dir_clone,
                    local_port,
                    backend_url,
                    status_tx,
                    tasks_tx,
                    kill_rx,
                    quit_rx
                ).await;
            });
            std::process::exit(0);
        });
    }

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
            // COM is initialized here by winit
            let mut has_tray = false;
            let _tray_handle = match tray::build_tray(state.clone(), config_dir.clone(), local_port, ui_ctx.clone()) {
                Ok((handle, internal_quit_rx)) => {
                    // We need to forward internal_quit_rx to quit_tx
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let _ = internal_quit_rx.await;
                            let _ = quit_tx.send(());
                        });
                    });
                    has_tray = true;
                    Some(handle)
                }
                Err(e) => {
                    tracing::error!("Failed to build tray: {}", e);
                    // Prevent quit_tx from dropping so the background daemon doesn't exit!
                    Box::leak(Box::new(quit_tx));
                    None
                }
            };
            
            let app = ui::BridgeApp::new(cc, status_rx, tasks_rx, kill_tx, is_already_running, has_tray, config_dir.clone());
            let mut w_ctx = ui_ctx.blocking_write();
            *w_ctx = Some(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    ).map_err(|e| DaemonError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

    Ok(())
}

async fn background_daemon_task(
    state: Arc<RwLock<DaemonState>>,
    config_dir: PathBuf,
    local_port: u16,
    backend_url: String,
    status_tx: tokio::sync::watch::Sender<String>,
    tasks_tx: tokio::sync::watch::Sender<Vec<daemon_core::task_registry::TaskState>>,
    mut kill_rx: tokio::sync::mpsc::Receiver<uuid::Uuid>,
    quit_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let is_paired = {
        let s = state.read().await;
        s.device_id.is_some()
    };

    use daemon_core::task_registry::TaskRegistry;
    let task_registry = Arc::new(tokio::sync::RwLock::new(TaskRegistry::new(200)));

    tokio::spawn({
        let tr = task_registry.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                interval.tick().await;
                let mut reg = tr.write().await;
                reg.cleanup_stale_tasks(std::time::Duration::from_secs(5));
                let states: Vec<_> = reg.states().cloned().collect();
                let _ = tasks_tx.send(states);
            }
        }
    });

    tokio::spawn({
        let tr = task_registry.clone();
        async move {
            while let Some(id) = kill_rx.recv().await {
                let mut reg = tr.write().await;
                reg.kill_task(id).await;
            }
        }
    });

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
                        1, 
                        &mut sd,
                        ptr::null_mut(),
                    ) != 0 {
                        use interprocess::os::windows::security_descriptor::BorrowedSecurityDescriptor;
                        let bsd = BorrowedSecurityDescriptor::from_ptr(sd);
                        if let Ok(owned_sd) = bsd.to_owned_sd() {
                            options = options.security_descriptor(owned_sd);
                        }
                    }
                }
            }

            #[cfg(unix)]
            {
                use interprocess::os::unix::local_socket::ListenerOptionsExt;
                options = options.mode(0o600);
            }

            if let Ok(listener) = options.create_tokio() {
                loop {
                    if let Ok(mut stream) = listener.accept().await {
                        let state_clone = state_ipc.clone();
                        let backend_url_clone = backend_url_ipc.clone();
                        let config_dir_clone = config_dir_ipc.clone();
                        
                        tokio::spawn(async move {
                            let mut buf = [0u8; 1024];
                            if let Ok(len) = stream.read(&mut buf).await {
                                let msg = String::from_utf8_lossy(&buf[..len]).to_string();
                                if let Some(uri) = msg.strip_prefix("synthhires://") {
                                    let clean_uri = uri.trim_end_matches('/').trim();
                                    let clean_token = if let Some(t) = clean_uri.strip_prefix("pair?token=") {
                                        t
                                    } else {
                                        clean_uri
                                    };
                                    
                                    if let Ok(_) = daemon_core::keyring::TokenStore::save(clean_token, clean_token) {
                                        let mut s = state_clone.write().await;
                                        s.device_id = Some(clean_token.to_string());
                                        let _ = s.save(&config_dir_clone).await;
                                        
                                        let ws_state = state_clone.clone();
                                        let ws_backend = backend_url_clone;
                                        tokio::spawn(async move {
                                            let _ = run_ws_client(ws_state, ws_backend).await;
                                        });
                                        let _ = stream.write_all(b"ACK").await;
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
    });

    tokio::spawn({
        let state_http = state.clone();
        let config_dir_http = config_dir.clone();
        let backend_url_http = backend_url.clone();
        let status_tx_http = status_tx.clone();
        async move {
            use axum::{routing::{get, post}, Router, Json, http::Method};
            use tower_http::cors::{CorsLayer, Any};
            use serde::{Deserialize, Serialize};

            #[derive(Serialize)]
            struct StatusRes { paired: bool }
            
            #[derive(Deserialize)]
            struct PairReq { 
                token: String,
                backend_url: Option<String>,
            }
            
            #[derive(Serialize)]
            struct PairRes { success: bool }

            let cors = CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(Any);

            let app = Router::new()
                .route("/status", get({
                    let s = state_http.clone();
                    move || async move {
                        let is_paired = s.read().await.device_id.is_some();
                        Json(StatusRes { paired: is_paired })
                    }
                }))
                .route("/pair", post({
                    let s = state_http.clone();
                    let c = config_dir_http.clone();
                    let b = backend_url_http.clone();
                    move |Json(payload): Json<PairReq>| async move {
                        let token = payload.token;
                        if let Ok(_) = daemon_core::keyring::TokenStore::save(&token, &token) {
                            let mut st = s.write().await;
                            st.device_id = Some(token.clone());
                            if let Some(ref bu) = payload.backend_url {
                                st.backend_url = Some(bu.clone());
                            }
                            let _ = st.save(&c).await;
                            
                            let ws_state = s.clone();
                            let ws_backend = payload.backend_url.unwrap_or(b.clone());
                            tokio::spawn(async move {
                                let _ = run_ws_client(ws_state, ws_backend).await;
                            });
                            Json(PairRes { success: true })
                        } else {
                            Json(PairRes { success: false })
                        }
                    }
                }))
                .layer(cors);

            match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", local_port)).await {
                Ok(listener) => {
                    tracing::info!("Local HTTP server running on port {}", local_port);
                    let _ = axum::serve(listener, app).await;
                }
                Err(e) => {
                    tracing::error!("Failed to bind local server on port {}: {}", local_port, e);
                    let _ = status_tx_http.send(format!("Error: No se pudo abrir el puerto local {} ({})", local_port, e));
                }
            }
        }
    });

    if is_paired {
        let ws_state = state.clone();
        let ws_backend = {
            let s = state.read().await;
            s.backend_url.clone().unwrap_or(backend_url.clone())
        };
        let status_tx_ws = status_tx.clone();
        tokio::spawn(async move {
            match run_ws_client(ws_state, ws_backend, status_tx_ws.clone()).await {
                Ok(_) => {
                    let _ = status_tx_ws.send("Desconectado de la web".into());
                }
                Err(e) => {
                    tracing::error!("WS client died: {e}");
                    let _ = status_tx_ws.send(format!("Error WS: {}", e));
                }
            }
        });
    } else {
        let _ = status_tx.send("Listo para emparejar (abre la web)".into());
    }

    // Wait until user quits
    let _ = quit_rx.await;
    tracing::info!("tokio background daemon exiting");
}

async fn run_ws_client(
    state: Arc<RwLock<DaemonState>>, 
    backend_url: String,
    status_tx: tokio::sync::watch::Sender<String>
) -> Result<()> {
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
        Some(status_tx)
    );
    ws.run().await
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

