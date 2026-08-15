//! Filesystem operations.
//!
//! Read, write, delete — all gated by `CapabilityGate`. Writes go
//! through a temp file + atomic rename so a crash mid-write never
//! leaves a corrupted file behind.

use crate::{
    capability::{CapabilityGate, GateDecision},
    DaemonError, Result,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct FsReadRequest {
    pub path: PathBuf,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FsReadResult {
    pub content_base64: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FsWriteRequest {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FsWriteResult {
    pub bytes_written: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FsDeleteRequest {
    pub path: PathBuf,
}

pub struct FsOps<'a> {
    gate: &'a CapabilityGate,
}

impl<'a> FsOps<'a> {
    pub fn new(gate: &'a CapabilityGate) -> Self {
        Self { gate }
    }

    pub async fn read(&self, req: FsReadRequest) -> Result<FsReadResult> {
        self.gate_for_path("desktop.fs.read", &req.path)?;
        let max = req.max_bytes.unwrap_or(1_048_576).min(10_485_760);
        let bytes = fs::read(&req.path).await.map_err(DaemonError::Io)?;
        let truncated = if bytes.len() as u64 > max {
            &bytes[..max as usize]
        } else {
            &bytes[..]
        };
        Ok(FsReadResult {
            content_base64: base64_encode(truncated),
            size: truncated.len() as u64,
        })
    }

    pub async fn write(&self, req: FsWriteRequest) -> Result<FsWriteResult> {
        self.gate_for_path("desktop.fs.write", &req.path)?;
        let parent = req
            .path
            .parent()
            .ok_or_else(|| DaemonError::PathDenied(format!("{}: no parent", req.path.display())))?;
        fs::create_dir_all(parent).await.map_err(DaemonError::Io)?;
        let tmp = req.path.with_extension("synthhires-tmp");
        fs::write(&tmp, req.content.as_bytes())
            .await
            .map_err(DaemonError::Io)?;
        fs::rename(&tmp, &req.path).await.map_err(DaemonError::Io)?;
        // Empirical verification: read back what we just wrote. Only a
        // byte-identical file is reported as verified. This is the same
        // honesty guarantee as the web-side local tools.
        let written = fs::read(&req.path).await.map_err(DaemonError::Io)?;
        let verified = written == req.content.as_bytes();
        Ok(FsWriteResult {
            bytes_written: written.len() as u64,
            verified,
        })
    }

    pub async fn delete(&self, req: FsDeleteRequest) -> Result<()> {
        self.gate_for_path("desktop.fs.delete", &req.path)?;
        let meta = fs::metadata(&req.path).await.map_err(DaemonError::Io)?;
        if meta.is_dir() {
            fs::remove_dir_all(&req.path)
                .await
                .map_err(DaemonError::Io)?;
        } else {
            fs::remove_file(&req.path).await.map_err(DaemonError::Io)?;
        }
        Ok(())
    }

    fn gate_for_path(&self, capability: &str, path: &Path) -> Result<()> {
        match self.gate.check_path(capability, path) {
            GateDecision::Allow => Ok(()),
            GateDecision::RequireConsent => Err(DaemonError::CapabilityDenied(format!(
                "{} requires consent for {}",
                capability,
                path.display()
            ))),
            GateDecision::Deny => Err(DaemonError::CapabilityDenied(capability.into())),
        }
    }
}

/// Minimal base64 encoder so we don't pull in the `base64` crate.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
        i += 3;
    }
    if i < bytes.len() {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        if i + 1 < bytes.len() {
            let n = n | ((bytes[i + 1] as u32) << 8);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        } else {
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
    }
    out
}
