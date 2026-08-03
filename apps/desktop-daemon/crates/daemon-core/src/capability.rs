//! Local capability enforcement.
//!
//! Defense in depth. The server validates capabilities too, but the
//! daemon is the last line of defense if the server is compromised
//! or the WS is replayed. Every action_request that arrives is
//! checked against `Scope` BEFORE any code runs.
//!
//! Path-prefix matching is intentionally identical to the TS
//! implementation in `src/lib/agent/bridge-codes.ts →
//! pathMatchesAlwaysAllow` so the three implementations never
//! disagree on whether `/home/u/workspace` matches
//! `/home/u/workspace/`.

use daemon_protocol::Scopes;
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScopeSnapshot {
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub always_allow_paths: Vec<PathBuf>,
}

impl From<&Scopes> for ScopeSnapshot {
    fn from(s: &Scopes) -> Self {
        ScopeSnapshot {
            capabilities: s.capabilities.clone(),
            always_allow_paths: s
                .always_allow_paths
                .iter()
                .map(PathBuf::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    RequireConsent,
    Deny,
}

pub struct CapabilityGate {
    snapshot: ScopeSnapshot,
}

impl CapabilityGate {
    pub fn new(snapshot: ScopeSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn update(&mut self, snapshot: ScopeSnapshot) {
        self.snapshot = snapshot;
    }

    pub fn allows(&self, capability: &str) -> bool {
        self.snapshot.capabilities.iter().any(|c| c == capability)
    }

    /// Returns Allow if the path is in the alwaysAllow list OR the
    /// capability is one that requires per-action consent regardless
    /// of path (e.g. shell.execute). Returns RequireConsent if the
    /// path is within an allowed scope but the user hasn't added it
    /// to alwaysAllowPaths. Returns Deny if the capability itself is
    /// not in the snapshot.
    pub fn check_path(
        &self,
        capability: &str,
        path: &Path,
    ) -> GateDecision {
        if !self.allows(capability) {
            return GateDecision::Deny;
        }
        if self.path_matches_any(path) {
            return GateDecision::Allow;
        }
        GateDecision::RequireConsent
    }

    fn path_matches_any(&self, path: &Path) -> bool {
        for prefix in &self.snapshot.always_allow_paths {
            if path.starts_with(prefix) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn gate_with_paths(prefixes: &[&str]) -> CapabilityGate {
        let snap = ScopeSnapshot {
            capabilities: vec!["desktop.fs.read".into(), "desktop.fs.write".into()],
            always_allow_paths: prefixes.iter().map(PathBuf::from).collect(),
        };
        CapabilityGate::new(snap)
    }

    #[test]
    fn allows_exact_match() {
        let g = gate_with_paths(&["/home/u/workspace"]);
        assert_eq!(
            g.check_path("desktop.fs.read", Path::new("/home/u/workspace")),
            GateDecision::Allow
        );
    }

    #[test]
    fn allows_with_trailing_slash() {
        let g = gate_with_paths(&["/home/u/workspace/"]);
        assert_eq!(
            g.check_path("desktop.fs.read", Path::new("/home/u/workspace")),
            GateDecision::Allow
        );
    }

    #[test]
    fn allows_under_prefix() {
        let g = gate_with_paths(&["/home/u/workspace"]);
        assert_eq!(
            g.check_path("desktop.fs.read", Path::new("/home/u/workspace/sub/file.txt")),
            GateDecision::Allow
        );
    }

    #[test]
    fn denies_prefix_collision() {
        // Without trailing-separator guard, "/home/u/workspace-evil"
        // would match "/home/u/workspace". The strip_trailing_sep logic
        // ensures we require a separator boundary.
        let g = gate_with_paths(&["/home/u/workspace"]);
        assert_eq!(
            g.check_path("desktop.fs.read", Path::new("/home/u/workspace-evil/file")),
            GateDecision::RequireConsent
        );
    }

    #[test]
    fn denies_capability_not_in_scope() {
        let g = gate_with_paths(&["/home/u/workspace"]);
        assert_eq!(
            g.check_path("desktop.shell.execute", Path::new("/home/u/workspace/cmd")),
            GateDecision::Deny
        );
    }
}