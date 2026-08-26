//! Pairing flow glue: exchange a short-lived code for a device token.

use crate::{keyring::TokenStore, Result};
use daemon_protocol::Scopes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCompleteRequest {
    pub code: String,
    pub pairing_id: Option<String>,
    pub device_kind: &'static str,
    pub device_name: String,
    pub fingerprint: String,
    pub desired_scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCompleteData {
    pub device_id: String,
    pub token: String,
    pub token_expires_at: String,
    pub ws_url: String,
    pub scopes: Scopes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCompleteResponse {
    pub success: bool,
    pub data: PairCompleteData,
}

pub struct PairingFlow<'a> {
    backend_url: &'a str,
    client: &'a reqwest::Client,
}

impl<'a> PairingFlow<'a> {
    pub fn new(backend_url: &'a str, client: &'a reqwest::Client) -> Self {
        Self {
            backend_url,
            client,
        }
    }

    pub async fn complete(&self, mut req: PairCompleteRequest) -> Result<PairCompleteResponse> {
        // Keep CLI/deep-link pairing aligned with the web's default scope.
        // Existing callers can request a subset, but a Desktop client must
        // never silently omit a capability that this binary implements.
        if req.device_kind == "desktop" {
            for capability in [
                "desktop.shell.execute",
                "desktop.fs.read",
                "desktop.fs.write",
                "desktop.fs.delete",
                "desktop.fs.verify",
                "desktop.fs.list",
                "desktop.fs.watch",
                "desktop.process.list",
                "desktop.process.kill",
                "desktop.network.fetch",
                "sync.chat.push",
            ] {
                if !req.desired_scopes.iter().any(|value| value == capability) {
                    req.desired_scopes.push(capability.to_string());
                }
            }
        }

        let url = format!(
            "{}/api/devices/pair/complete",
            self.backend_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| crate::DaemonError::Protocol(format!("pair/complete http: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(crate::DaemonError::Protocol(format!(
                "pair/complete {status}: {body}"
            )));
        }

        let body: PairCompleteResponse = response
            .json()
            .await
            .map_err(|e| crate::DaemonError::Protocol(format!("pair/complete decode: {e}")))?;
        let ws_url = if body.data.ws_url.starts_with("ws://") || body.data.ws_url.starts_with("wss://") {
            body.data.ws_url.clone()
        } else {
            let origin = self
                .backend_url
                .trim_end_matches('/')
                .replace("https://", "wss://")
                .replace("http://", "ws://");
            format!("{}{}", origin, body.data.ws_url)
        };
        TokenStore::save(&body.data.device_id, &body.data.token)?;
        Ok(PairCompleteResponse {
            data: PairCompleteData { ws_url, ..body.data },
            success: body.success,
        })
    }
}

pub fn fingerprint_hash(hostname: &str, os: &str, machine_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(hostname.as_bytes());
    hash.update(b"|");
    hash.update(os.as_bytes());
    hash.update(b"|");
    hash.update(machine_id.as_bytes());
    hex::encode(hash.finalize())
}
