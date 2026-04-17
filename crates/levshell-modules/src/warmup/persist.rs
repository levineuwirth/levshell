//! Warmup trigger state persistence.
//!
//! A single JSON file under `$XDG_STATE_HOME/levshell/warmup.json`
//! holds the last-warmup timestamp. That's all — the rest of the
//! trigger state is in-memory (spec §2.12.1 is about live sessions,
//! not long-term analytics).
//!
//! Persistence is best-effort: read failures log and fall through to
//! "never fired", write failures log and are dropped. Neither blocks
//! the fire path.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedWarmupState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_warmup_at: Option<DateTime<Utc>>,
}

impl PersistedWarmupState {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<PersistedWarmupState>(&text) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "warmup: persisted state malformed, starting fresh",
                    );
                    PersistedWarmupState::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                PersistedWarmupState::default()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "warmup: failed to read state, starting fresh",
                );
                PersistedWarmupState::default()
            }
        }
    }

    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    dir = %parent.display(),
                    error = %e,
                    "warmup: cannot create state dir; state write dropped",
                );
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "warmup: state write failed",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "warmup: state serialization failed (unreachable)");
            }
        }
    }
}

/// Default state path: `$XDG_STATE_HOME/levshell/warmup.json`, falling
/// back to `$HOME/.local/state/levshell/warmup.json`.
pub fn default_warmup_state_path() -> PathBuf {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state).join("levshell/warmup.json");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/levshell/warmup.json");
    }
    PathBuf::from("/tmp/levshell-warmup.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_through_tempdir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/warmup.json");
        let now = Utc::now();
        let s = PersistedWarmupState {
            last_warmup_at: Some(now),
        };
        s.save(&path);
        let reloaded = PersistedWarmupState::load(&path);
        // Serialization rounds to microseconds; allow a tight delta.
        let delta = reloaded.last_warmup_at.unwrap() - now;
        assert!(delta.num_milliseconds().abs() < 10);
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempdir().unwrap();
        let s = PersistedWarmupState::load(&dir.path().join("absent.json"));
        assert!(s.last_warmup_at.is_none());
    }

    #[test]
    fn malformed_file_logs_and_yields_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("warmup.json");
        std::fs::write(&path, "not json").unwrap();
        let s = PersistedWarmupState::load(&path);
        assert!(s.last_warmup_at.is_none());
    }
}
