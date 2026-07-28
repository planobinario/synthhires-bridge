//! SynthHires Desktop Daemon — core library.
//!
//! Modules:
//!   • `capability` — local enforcement of the device's granted scope.
//!                     Defense in depth: even if the server is
//!                     compromised, the daemon refuses actions not in
//!                     the cached scopes.
//!   • `shell`      — executes commands with isolated args via
//!                     `std::process::Command` (NEVER string concat
//!                     into `sh -c "..."`).
//!   • `fs`         — read/write/delete with prefix-match allow-list.
//!   • `keyring`    — OS secure storage wrapper (Windows Credential
//!                     Manager / macOS Keychain / Linux Secret Service).
//!   • `fingerprint` — host + OS + machine-id hash, recomputed on
//!                     each hello so a moved disk doesn't masquerade.
//!   • `audit`      — encrypted local audit log. The server already
//!                     keeps an authoritative copy; this is for the
//!                     user to inspect offline and for forenscis.

pub mod capability;
pub mod shell;
pub mod fs_ops;
pub mod keyring;
pub mod fingerprint;
pub mod audit;
pub mod autoupdate;
pub mod pairing;
pub mod ws_client;
pub mod jni_android;

pub use capability::{CapabilityGate, GateDecision};
pub use shell::ShellRunner;
pub use fs_ops::FsOps;
pub use keyring::TokenStore;
pub use fingerprint::DeviceFingerprint;
pub use audit::AuditLog;
pub use autoupdate::{check_for_update, download_and_verify, SignedManifest, UpdateStatus};
pub use pairing::PairingFlow;
pub use ws_client::WsClient;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("capability not granted: {0}")]
    CapabilityDenied(String),
    #[error("path outside alwaysAllow: {0}")]
    PathDenied(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("websocket error: {0}")]
    Ws(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("user denied consent")]
    UserDenied,
    #[error("timed out after {0}ms")]
    Timeout(u64),
}

impl From<tungstenite::Error> for DaemonError {
    fn from(e: tungstenite::Error) -> Self {
        DaemonError::Ws(format!("{e}"))
    }
}

pub type Result<T> = std::result::Result<T, DaemonError>;