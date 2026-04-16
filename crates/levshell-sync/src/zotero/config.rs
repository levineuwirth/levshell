//! Zotero adapter configuration (`~/.config/levshell/sync/zotero.toml`).
//!
//! Minimal knobs: point at the Zotero SQLite database, set a poll
//! interval, optionally filter libraries. The default database path
//! mirrors Zotero's own default (`$HOME/Zotero/zotero.sqlite`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_POLL_SECS: u64 = 300;

#[derive(Debug, Error)]
pub enum ZoteroConfigError {
    #[error("reading zotero config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing zotero config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_poll_secs() -> u64 {
    DEFAULT_POLL_SECS
}

fn default_enabled() -> bool {
    true
}

/// Deserialized form of the TOML file. `database_path` is required; all
/// other fields have sensible defaults so a one-line config works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroConfig {
    /// Absolute path to `zotero.sqlite`. Opened read-only; Zotero's own
    /// process can be running at the same time (Zotero uses WAL, so
    /// concurrent readers are safe).
    pub database_path: PathBuf,

    /// Whether the adapter should run. Defaults to `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Polling interval in seconds. Zotero libraries change slowly —
    /// the default of 300s (5 min) matches the framework baseline.
    #[serde(default = "default_poll_secs")]
    pub poll_interval_secs: u64,

    /// Restrict sync to these Zotero library IDs. Empty (the default)
    /// means all libraries are synced. Library ID `1` is always the
    /// user's personal library; group libraries get higher IDs.
    #[serde(default)]
    pub libraries: Vec<i64>,
}

impl ZoteroConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs.max(1))
    }

    /// Whether `library_id` should be included in this sync. Returns
    /// `true` when no filter is configured.
    pub fn matches_library(&self, library_id: i64) -> bool {
        self.libraries.is_empty() || self.libraries.contains(&library_id)
    }

    pub fn load_from(path: &Path) -> Result<Self, ZoteroConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ZoteroConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| ZoteroConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zotero.toml");
        std::fs::write(&path, "database_path = \"/home/u/Zotero/zotero.sqlite\"\n").unwrap();
        let cfg = ZoteroConfig::load_from(&path).unwrap();
        assert_eq!(
            cfg.database_path.to_str().unwrap(),
            "/home/u/Zotero/zotero.sqlite"
        );
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, DEFAULT_POLL_SECS);
        assert!(cfg.libraries.is_empty());
    }

    #[test]
    fn parses_full_config_with_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zotero.toml");
        std::fs::write(
            &path,
            r#"
database_path = "/tmp/zotero.sqlite"
enabled = false
poll_interval_secs = 600
libraries = [1, 42]
"#,
        )
        .unwrap();
        let cfg = ZoteroConfig::load_from(&path).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 600);
        assert_eq!(cfg.libraries, vec![1, 42]);
    }

    #[test]
    fn matches_library_honors_filter() {
        let cfg = ZoteroConfig {
            database_path: PathBuf::from("/x"),
            enabled: true,
            poll_interval_secs: 300,
            libraries: vec![1],
        };
        assert!(cfg.matches_library(1));
        assert!(!cfg.matches_library(2));

        let unfiltered = ZoteroConfig {
            libraries: Vec::new(),
            ..cfg
        };
        assert!(unfiltered.matches_library(1));
        assert!(unfiltered.matches_library(99));
    }

    #[test]
    fn missing_database_path_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zotero.toml");
        std::fs::write(&path, "enabled = true\n").unwrap();
        let err = ZoteroConfig::load_from(&path).unwrap_err();
        assert!(matches!(err, ZoteroConfigError::Toml { .. }));
    }

    #[test]
    fn nonexistent_file_is_an_io_error() {
        let err = ZoteroConfig::load_from(Path::new("/nope/zotero.toml")).unwrap_err();
        assert!(matches!(err, ZoteroConfigError::Io { .. }));
    }
}
