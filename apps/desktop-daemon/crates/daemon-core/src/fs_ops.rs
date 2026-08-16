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

#[derive(Debug, Clone, Deserialize)]
pub struct FsVerifyRequest {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct FsVerifyResult {
    pub exists: bool,
    pub is_dir: bool,
    pub readable: bool,
    pub writable: bool,
    pub error: Option<String>,
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

    /// Empirical access verification: the agent asks "can you really
    /// work in this folder?" and the daemon proves it on disk. Returns
    /// a full per-axis report instead of guessing:
    ///   • exists   — the path resolves on this machine
    ///   • is_dir   — it is a directory (the expected shape)
    ///   • readable — a directory listing works
    ///   • writable — a probe file was created AND read back AND
    ///                removed; the probe never touches user files.
    /// Failure never throws — every axis degrades to a field so the
    /// web UI can explain exactly what's wrong.
    pub async fn verify(&self, req: FsVerifyRequest) -> FsVerifyResult {
        match fs::metadata(&req.path).await {
            Err(e) => FsVerifyResult {
                exists: false,
                is_dir: false,
                readable: false,
                writable: false,
                error: Some(format!("no existe o no es accesible: {e}")),
            },
            Ok(meta) => {
                let is_dir = meta.is_dir();
                let readable = if is_dir {
                    fs::read_dir(&req.path).await.is_ok()
                } else {
                    fs::read(&req.path).await.is_ok()
                };
                let writable = if is_dir {
                    self.probe_write(&req.path).await
                } else {
                    let parent = req.path.parent().unwrap_or_else(|| {
                        std::path::Path::new(&req.path).parent().unwrap_or(std::path::Path::new("."))
                    });
                    self.probe_write(parent).await
                };
                FsVerifyResult {
                    exists: true,
                    is_dir,
                    readable,
                    writable,
                    error: if readable && writable {
                        None
                    } else {
                        Some(format!(
                            "lectura: {}, escritura: {}",
                            if readable { "ok" } else { "FALLA" },
                            if writable { "ok" } else { "FALLA" },
                        ))
                    },
                }
            }
        }
    }

    /// Create a unique probe file, read it back byte-for-byte, then
    /// remove it. Any step failing leaves `writable=false` — and never
    /// leaves the probe behind.
    async fn probe_write(&self, dir: &std::path::Path) -> bool {
        let probe_name = format!(".synthhires-verify-{}.tmp", uuid::Uuid::new_v4());
        let probe = dir.join(probe_name);
        let payload = format!("synthhires-verify:{}", uuid::Uuid::new_v4());
        if fs::write(&probe, payload.as_bytes()).await.is_err() {
            return false;
        }
        let read_back = match fs::read(&probe).await {
            Ok(b) => b == payload.as_bytes(),
            Err(_) => false,
        };
        let _ = fs::remove_file(&probe).await;
        read_back
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
