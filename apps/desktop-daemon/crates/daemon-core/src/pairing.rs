//! Pairing flow glue.
//!
//! Walks the user through:
//!   1. Show a pairing code entry dialog (the 6-char code the user
//!      reads off the web UI).
//!   2. POST /api/devices/pair/complete with fingerprint + name + scope.
//!   3. Receive deviceId + raw token.
//!   4. Persist both to the keyring + state.json.

use crate::{keyring::TokenStore, Result};
use daemon_protocol::Scopes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCompleteRequest {
    pub code: String,
    pub pairing_id: Option<String>,
    pub device_kind: &'static str,
    pub device_name: String,
    pub fingerprint: String,
    pub desired_scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCompleteResponse {
    pub device_id: String,
    pub token: String,
    pub token_expires_at: String,
    pub ws_url: String,
    pub scopes: Scopes,
}

pub struct PairingFlow<'a> {
    backend_url: &'a str,
    client: &'a reqwest::Client,
}

impl<'a> PairingFlow<'a> {
    pub fn new(backend_url: &'a str, client: &'a reqwest::Client) -> Self {
        Self { backend_url, client }
    }

    /// POST to /api/devices/pair/complete. Returns the response or an
    /// error that the daemon CLI surfaces to the user.
    pub async fn complete(
        &self,
        req: PairCompleteRequest,
    ) -> Result<PairCompleteResponse> {
        let url = format!("{}/api/devices/pair/complete", self.backend_url);
        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| crate::DaemonError::Protocol(format!("pair/complete http: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(crate::DaemonError::Protocol(format!(
                "pair/complete {status}: {body}"
            )));
        }
        let body: PairCompleteResponse = resp
            .json()
            .await
            .map_err(|e| crate::DaemonError::Protocol(format!("pair/complete decode: {e}")))?;
        // Persist the token to the OS keyring; the state.json holds
        // the deviceId + scopes. The raw token never touches disk in
        // plaintext.
        TokenStore::save(&body.device_id, &body.token)?;
        Ok(body)
    }
}

pub fn fingerprint_hash(hostname: &str, os: &str, machine_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(hostname.as_bytes());
    h.update(b"|");
    h.update(os.as_bytes());
    h.update(b"|");
    h.update(machine_id.as_bytes());
    hex::encode(h.finalize())
}