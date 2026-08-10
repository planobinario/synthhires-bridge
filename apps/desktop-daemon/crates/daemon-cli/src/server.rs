use axum::{
    extract::State,
    http::{Method, StatusCode, header},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::DaemonState;

#[derive(Clone)]
pub struct ServerState {
    pub daemon_state: Arc<RwLock<DaemonState>>,
    pub config_dir: std::path::PathBuf,
    pub _backend_url: String,
    pub pairing_nonce: Arc<RwLock<Option<(String, std::time::Instant)>>>,
    pub status_tx: tokio::sync::watch::Sender<String>,
    pub last_poll: Arc<std::sync::atomic::AtomicU64>,
    pub ws_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

#[derive(Serialize)]
pub struct StatusResponse {
    paired: bool,
    nonce: Option<String>,
}

#[derive(Deserialize)]
pub struct PairRequest {
    token: String,
    backend_url: String,
    nonce: String,
}

pub async fn start_http_server(
    state: ServerState,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Generar un nonce único al arrancar (solo para emparejamientos nuevos)
    {
        let is_paired = state.daemon_state.read().await.device_id.is_some();
        if !is_paired {
            let mut w = state.pairing_nonce.write().await;
            *w = Some((uuid::Uuid::new_v4().to_string(), std::time::Instant::now()));
        }
    }

    // CORS estricto con PNA (Private Network Access)
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            let o = origin.as_bytes();
            o == b"https://app.synthhires.com" || o == b"http://localhost:4321" || o == b"http://127.0.0.1:4321" || o == b"http://localhost:8787" || o == b"http://127.0.0.1:8787"
        }))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE])
        // Cabecera esencial para PNA en navegadores basados en Chromium
        .allow_private_network(true);

    let app = Router::new()
        .route("/status", get(handle_status))
        .route("/pair", post(handle_pair))
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("Starting secure local HTTP server on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn check_origin(headers: &axum::http::HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    if origin != "https://app.synthhires.com" && origin != "http://localhost:4321" && origin != "http://127.0.0.1:4321" && origin != "http://localhost:8787" && origin != "http://127.0.0.1:8787" {
        tracing::warn!("Rejecting request from invalid origin: {}", origin);
        return Err((StatusCode::FORBIDDEN, "Invalid Origin"));
    }
    Ok(())
}

async fn handle_status(
    headers: axum::http::HeaderMap,
    State(state): State<ServerState>,
) -> Result<Json<StatusResponse>, (StatusCode, &'static str)> {
    check_origin(&headers)?;
    // Registrar el poll del frontend para saber que la UI está abierta
    state.last_poll.store(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        std::sync::atomic::Ordering::Relaxed
    );

    let is_paired = state.daemon_state.read().await.device_id.is_some();
    
    // Si ya está emparejado, no exponemos nonce.
    // Si no está emparejado, comprobamos el TTL del nonce. Si expira o no hay, creamos uno nuevo.
    let nonce = if !is_paired {
        let mut n = state.pairing_nonce.write().await;
        let now = std::time::Instant::now();
        if let Some((_, created_at)) = &*n {
            if now.duration_since(*created_at) > std::time::Duration::from_secs(120) {
                // Expired
                *n = None;
            }
        }
        if n.is_none() {
            *n = Some((uuid::Uuid::new_v4().to_string(), now));
        }
        n.as_ref().map(|(val, _)| val.clone())
    } else {
        None
    };

    Ok(Json(StatusResponse {
        paired: is_paired,
        nonce,
    }))
}

async fn handle_pair(
    headers: axum::http::HeaderMap,
    State(state): State<ServerState>,
    Json(payload): Json<PairRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, &'static str)> {
    check_origin(&headers)?;
    let is_paired = state.daemon_state.read().await.device_id.is_some();
    if is_paired {
        return Err((StatusCode::CONFLICT, "Already paired"));
    }

    // Validar el Nonce
    {
        let mut n = state.pairing_nonce.write().await;
        let valid = match &*n {
            Some((val, created_at)) => {
                val == &payload.nonce && std::time::Instant::now().duration_since(*created_at) <= std::time::Duration::from_secs(120)
            }
            None => false,
        };
        if !valid {
            tracing::warn!("Rejecting /pair request: invalid, expired, or missing nonce");
            return Err((StatusCode::FORBIDDEN, "Invalid nonce"));
        }
        // Consumir el nonce para que no se pueda reusar (previene replay attacks)
        *n = None;
    }

    tracing::info!("Valid pairing request received via local HTTP. Token handoff starting...");

    // Guardar token en el llavero local (Keyring)
    if daemon_core::keyring::TokenStore::save(&payload.token, &payload.token).is_err() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to save token to keyring"));
    }

    // Actualizar estado
    {
        let mut s = state.daemon_state.write().await;
        s.device_id = Some(payload.token.clone());
        s.backend_url = Some(payload.backend_url.clone());
        let _ = s.save(&state.config_dir).await;
    }

    // Iniciar el WS Client de forma asíncrona
    let ws_state = state.daemon_state.clone();
    let status_tx = state.status_tx.clone();
    let mut hw = state.ws_handle.lock().await;
    if let Some(h) = hw.take() {
        h.abort();
    }
    *hw = Some(tokio::spawn(async move {
        let _ = status_tx.send("Conectando al servidor...".to_string());
        let tx_for_ws = status_tx.clone();
        if let Err(e) = crate::run_ws_client(ws_state, payload.backend_url, tx_for_ws).await {
            tracing::error!("WS client died: {e}");
            let _ = status_tx.send(format!("Error de conexión: {e}"));
        }
    }));

    Ok(Json(serde_json::json!({ "success": true })))
}
