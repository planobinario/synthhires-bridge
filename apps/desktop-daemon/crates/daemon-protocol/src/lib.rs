//! Mirror of `src/lib/agent/bridge-protocol.ts`.
//!
//! This crate is the single source of truth for the Rust wire format.
//! The TypeScript module in the main repo MUST be regenerated from
//! here when shapes change. The conversion is mechanical: serde
//! rename_all = "camelCase" matches the JSON field names exactly.
//!
//! Protocol version is hard-coded to 1; breaking changes require
//! bumping to 2 in BOTH crates in lockstep (CI guard).

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BridgeFrame {
    Hello(HelloFrame),
    HelloAck(HelloAckFrame),
    Heartbeat(HeartbeatFrame),
    HeartbeatAck(HeartbeatAckFrame),
    ActionRequest(ActionRequestFrame),
    ActionCancel(ActionCancelFrame),
    ActionResult(ActionResultFrame),
    ActionStream(ActionStreamFrame),
    ConsentPrompt(ConsentPromptFrame),
    ConsentResponse(ConsentResponseFrame),
    ScopeUpdate(ScopeUpdateFrame),
    Resume(ResumeFrame),
    Revoke(RevokeFrame),
    Error(ErrorFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloFrame {
    pub v: u32,
    pub token_hash: String,
    pub fingerprint: String,
    pub device_kind: DeviceKind,
    pub device_name: String,
    pub client_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAckFrame {
    pub v: u32,
    pub device_id: String,
    pub scopes: Scopes,
    pub heartbeat_interval_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scopes {
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub always_allow_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatFrame {
    pub v: u32,
    pub t: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatAckFrame {
    pub v: u32,
    pub t: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRequestFrame {
    pub v: u32,
    pub id: String,
    pub capability: String,
    pub params: serde_json::Value,
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub skip_consent_prompt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionCancelFrame {
    pub v: u32,
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResultFrame {
    pub v: u32,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ActionError>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionStreamFrame {
    pub v: u32,
    pub id: String,
    pub seq: u64,
    pub channel: StreamChannel,
    pub data: String,
    #[serde(default)]
    pub eof: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StreamChannel {
    Stdout,
    Stderr,
    Log,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentPromptFrame {
    pub v: u32,
    pub id: String,
    pub capability: String,
    pub summary: String,
    pub params_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentResponseFrame {
    pub v: u32,
    pub id: String,
    pub approved: bool,
    pub remember: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeUpdateFrame {
    pub v: u32,
    pub scopes: Scopes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeFrame {
    pub v: u32,
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeFrame {
    pub v: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorFrame {
    pub v: u32,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<u16>,
}

/// Close codes from src/lib/agent/bridge-protocol.ts BRIDGE_CLOSE_CODES.
pub mod close_codes {
    pub const NORMAL: u16 = 1000;
    pub const GOING_AWAY: u16 = 1001;
    pub const AUTH_FAILED: u16 = 4001;
    pub const CAPABILITY_NOT_GRANTED: u16 = 4003;
    pub const RATE_LIMITED: u16 = 4029;
    pub const REVOKED: u16 = 4401;
    pub const PROTOCOL_MISMATCH: u16 = 4400;
}

/// Crockford-base32 used by the TS `bridge-codes.ts`. Both endpoints
/// of the bridge surface use this so the QR code in the web UI is
/// decodable by the Rust daemon without a translation step.
pub const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generate a 6-char pairing code (30 bits of entropy). MUST match
/// the algorithm in `src/lib/agent/bridge-codes.ts → toBase32`.
pub fn generate_pairing_code() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 5];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    to_crockford(&bytes, 6)
}

pub fn to_crockford(bytes: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(CROCKFORD[((buffer >> bits) & 0x1f) as usize] as char);
            if out.len() == chars {
                return out;
            }
        }
    }
    if bits > 0 && out.len() < chars {
        out.push(CROCKFORD[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}