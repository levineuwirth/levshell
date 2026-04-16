//! AnkiConnect adapter configuration
//! (`~/.config/levshell/sync/ankiconnect.toml`).
//!
//! AnkiConnect listens on `localhost:8765` by default. Remote /
//! non-default ports are rare but supported via `endpoint`. The
//! optional `api_key` field forwards to every request when the user
//! has configured AnkiConnect's key-auth feature.
//!
//! `deck_filter` accepts AnkiConnect search syntax (the same string
//! you'd type into Anki's browser search bar). Leave empty to sync
//! every non-suspended card.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8765";
const DEFAULT_POLL_SECS: u64 = 300;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Error)]
pub enum AnkiConnectConfigError {
    #[error("reading ankiconnect config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing ankiconnect config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.to_string()
}
fn default_enabled() -> bool {
    true
}
fn default_poll_secs() -> u64 {
    DEFAULT_POLL_SECS
}
fn default_request_timeout_secs() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}
fn default_deck_filter() -> String {
    "-is:suspended".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnkiConnectConfig {
    /// HTTP endpoint where AnkiConnect is listening. Defaults to
    /// `http://127.0.0.1:8765`.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Whether the adapter runs. `false` keeps the config around but
    /// disables scheduling.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Poll interval in seconds. Anki libraries change slowly outside
    /// review sessions — 5 min matches the framework baseline.
    #[serde(default = "default_poll_secs")]
    pub poll_interval_secs: u64,

    /// Per-request HTTP timeout. Large libraries can take a few
    /// seconds for `cardsInfo`; 15s is a comfortable default.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// AnkiConnect search query. Defaults to `-is:suspended` (every
    /// non-suspended card in every deck). Override to narrow scope,
    /// e.g. `"deck:Research OR deck:Languages"`.
    #[serde(default = "default_deck_filter")]
    pub deck_filter: String,

    /// Optional API key. AnkiConnect's key-auth feature forwards this
    /// as the `key` field on every request.
    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for AnkiConnectConfig {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            enabled: default_enabled(),
            poll_interval_secs: default_poll_secs(),
            request_timeout_secs: default_request_timeout_secs(),
            deck_filter: default_deck_filter(),
            api_key: None,
        }
    }
}

impl AnkiConnectConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs.max(1))
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    pub fn load_from(path: &Path) -> Result<Self, AnkiConnectConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| AnkiConnectConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| AnkiConnectConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let cfg = AnkiConnectConfig::default();
        assert_eq!(cfg.endpoint, "http://127.0.0.1:8765");
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 300);
        assert_eq!(cfg.deck_filter, "-is:suspended");
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn parses_empty_file_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ankiconnect.toml");
        std::fs::write(&path, "").unwrap();
        let cfg = AnkiConnectConfig::load_from(&path).unwrap();
        assert_eq!(cfg.endpoint, "http://127.0.0.1:8765");
    }

    #[test]
    fn parses_full_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ankiconnect.toml");
        std::fs::write(
            &path,
            r#"
endpoint = "http://localhost:9999"
enabled = false
poll_interval_secs = 60
deck_filter = "deck:Research"
api_key = "secret"
"#,
        )
        .unwrap();
        let cfg = AnkiConnectConfig::load_from(&path).unwrap();
        assert_eq!(cfg.endpoint, "http://localhost:9999");
        assert!(!cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 60);
        assert_eq!(cfg.deck_filter, "deck:Research");
        assert_eq!(cfg.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = AnkiConnectConfig::load_from(Path::new("/nope/ankiconnect.toml")).unwrap_err();
        assert!(matches!(err, AnkiConnectConfigError::Io { .. }));
    }
}
