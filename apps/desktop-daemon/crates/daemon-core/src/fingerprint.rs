//! Device fingerprint computation.
//!
//! SHA-256(hostname || os || machineId) — sent to the server during
//! pairing and on every hello so the user can recognise the device
//! in /space/connections ("MacBook de Marta · Darwin · …").
//!
//! The fingerprint is NOT a security boundary; an attacker who
//! controls those three strings trivially reproduces the hash. The
//! server only uses it to de-dupe ("this device has already been
//! paired; do you want to update its scope?") and to label entries
//! in the UI.

use sha2::{Digest, Sha256};

pub struct DeviceFingerprint {
    pub hostname: String,
    pub os: String,
    pub machine_id: String,
}

impl DeviceFingerprint {
    pub fn collect() -> Self {
        Self {
            hostname: hostname(),
            os: std::env::consts::OS.to_string(),
            machine_id: machine_id().unwrap_or_default(),
        }
    }

    pub fn hash_hex(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.hostname.as_bytes());
        hasher.update(b"|");
        hasher.update(self.os.as_bytes());
        hasher.update(b"|");
        hasher.update(self.machine_id.as_bytes());
        hex::encode(hasher.finalize())
    }
}

#[cfg(target_os = "windows")]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn hostname() -> String {
    std::process::Command::new("scutil")
        .arg("--get")
        .arg("ComputerName")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| std::env::var("HOST").unwrap_or_default())
}

#[cfg(target_os = "linux")]
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| std::env::var("HOSTNAME").unwrap_or_default())
}

#[cfg(target_os = "windows")]
fn machine_id() -> Option<String> {
    // HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid is the canonical
    // stable id on Windows; reading it requires winreg which we
    // deliberately keep out of the dependency tree for now.
    None
}

#[cfg(target_os = "macos")]
fn machine_id() -> Option<String> {
    std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find(|l| l.contains("IOPlatformUUID"))
                .and_then(|l| l.split('"').nth(3))
                .map(|s| s.to_string())
        })
}

#[cfg(target_os = "linux")]
fn machine_id() -> Option<String> {
    std::fs::read_to_string("/var/lib/dbus/machine-id")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/machine-id").ok())
        .map(|s| s.trim().to_string())
}