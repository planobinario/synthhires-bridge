use axum::{
    extract::State,
    http::{HeaderValue, Method, StatusCode, header},
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
    pub pairing_nonce: Arc<RwLock<Option<String>>>,
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
            *w = Some(uuid::Uuid::new_v4().to_string());
        }
    }

    // CORS estricto con PNA (Private Network Access)
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            let o = origin.as_bytes();
            o == b"https://app.synthhires.com" || o == b"http://localhost:4321" || o == b"http://127.0.0.1:4321"
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

async fn handle_status(State(state): State<ServerState>) -> Json<StatusResponse> {
    let is_paired = state.daemon_state.read().await.device_id.is_some();
    
    // Si ya está emparejado, no exponemos nonce.
    // Si no está emparejado y no hay nonce, lo creamos.
    let nonce = if !is_paired {
        let mut n = state.pairing_nonce.write().await;
        if n.is_none() {
            *n = Some(uuid::Uuid::new_v4().to_string());
        }
        n.clone()
    } else {
        None
    };

    Json(StatusResponse {
        paired: is_paired,
        nonce,
    })
}

async fn handle_pair(
    State(state): State<ServerState>,
    Json(payload): Json<PairRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, &'static str)> {
    let is_paired = state.daemon_state.read().await.device_id.is_some();
    if is_paired {
        return Err((StatusCode::CONFLICT, "Already paired"));
    }

    // Validar el Nonce
    {
        let mut n = state.pairing_nonce.write().await;
        if n.as_deref() != Some(payload.nonce.as_str()) {
            tracing::warn!("Rejecting /pair request: invalid or missing nonce");
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
        let _ = s.save(&state.config_dir).await;
    }

    // Iniciar el WS Client de forma asíncrona
    let ws_state = state.daemon_state.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::run_ws_client(ws_state, payload.backend_url).await {
            tracing::error!("WS client died: {e}");
        }
    });

    Ok(Json(serde_json::json!({ "success": true })))
}
