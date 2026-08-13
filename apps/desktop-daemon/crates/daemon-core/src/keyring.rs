//! OS keyring-backed token storage.
//!
//! The raw `deviceToken` returned by the server at /pair/complete is
//! stored ONLY in the OS-managed secure store:
//!   • Windows  : Credential Manager
//!   • macOS    : Keychain
//!   • Linux    : Secret Service (gnome-keyring, KWallet)
//!
//! The hash on the server side is enough for verification; the raw
//! token never touches disk in plaintext. We tag entries with the
//! `BRIDGE` service name and a per-device account so multiple
//! devices on the same machine coexist without collision.

use crate::{DaemonError, Result};
use keyring::Entry;

const SERVICE: &str = "com.synthhires.bridge.desktop";

pub struct TokenStore;

impl TokenStore {
    pub fn save(device_id: &str, token: &str) -> Result<()> {
        let entry =
            Entry::new(SERVICE, device_id).map_err(|e| DaemonError::Keyring(e.to_string()))?;
        entry
            .set_password(token)
            .map_err(|e| DaemonError::Keyring(e.to_string()))?;
        Ok(())
    }

    pub fn load(device_id: &str) -> Result<Option<String>> {
        let entry =
            Entry::new(SERVICE, device_id).map_err(|e| DaemonError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(t) => Ok(Some(t)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(DaemonError::Keyring(e.to_string())),
        }
    }

    pub fn delete(device_id: &str) -> Result<()> {
        let entry =
            Entry::new(SERVICE, device_id).map_err(|e| DaemonError::Keyring(e.to_string()))?;
        match entry.delete_password() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(DaemonError::Keyring(e.to_string())),
        }
    }

    pub fn list_device_ids() -> Result<Vec<String>> {
        // `keyring` 2.x doesn't ship a portable "list all" across
        // platforms. The CLI uses a sidecar file at
        // ~/.config/synthhires/devices.toml for the deviceId list
        // (one line per paired device). The keyring itself stores the
        // token per deviceId; the sidecar is metadata only.
        Ok(vec![])
    }
}
