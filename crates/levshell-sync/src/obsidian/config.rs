//! Obsidian adapter configuration (`~/.config/levshell/sync/obsidian.toml`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_POLL_SECS: u64 = 60;

/// Errors produced by [`ObsidianConfig::load_from`]. Other adapter-side
/// errors still surface through [`crate::SyncError`].
#[derive(Debug, Error)]
pub enum ObsidianConfigError {
    #[error("reading obsidian config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing obsidian config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_poll_secs() -> u64 {
    DEFAULT_POLL_SECS
}

fn default_exclude_dirs() -> Vec<String> {
    vec![".obsidian".into(), ".trash".into(), ".git".into()]
}

fn default_enabled() -> bool {
    true
}

/// Deserialized form of the TOML file. `vault_path` is required; every
/// other field has a sensible default so users can write a one-line config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObsidianConfig {
    /// Absolute path to the Obsidian vault directory. Must exist at
    /// adapter construction time — probe() reports `Unavailable` if not.
    pub vault_path: PathBuf,

    /// Whether the adapter should run. Defaults to `true`; users set
    /// `enabled = false` to keep the config around while the daemon
    /// skips scheduling the adapter.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Polling interval in seconds. Filesystem walks are cheap for
    /// reasonable vaults (<10k files), so the default of 60s is lower
    /// than the sync framework's 5-minute baseline.
    #[serde(default = "default_poll_secs")]
    pub poll_interval_secs: u64,

    /// Directory names to skip during the vault walk. Applied per
    /// component, not as a full-path match — `.obsidian` excludes every
    /// `.obsidian` directory regardless of depth.
    #[serde(default = "default_exclude_dirs")]
    pub exclude_dirs: Vec<String>,
}

impl ObsidianConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs.max(1))
    }

    pub fn is_excluded_dir(&self, name: &str) -> bool {
        self.exclude_dirs.iter().any(|e| e == name)
    }

    /// Load an `ObsidianConfig` from a TOML file. Typically called with
    /// `$XDG_CONFIG_HOME/levshell/sync/obsidian.toml`.
    pub fn load_from(path: &Path) -> Result<Self, ObsidianConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ObsidianConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| ObsidianConfigError::Toml {
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
        let path = dir.path().join("obsidian.toml");
        std::fs::write(
            &path,
            "vault_path = \"/home/user/vault\"\n",
        )
        .unwrap();
        let cfg = ObsidianConfig::load_from(&path).unwrap();
        assert_eq!(cfg.vault_path.to_str().unwrap(), "/home/user/vault");
        assert!(cfg.enabled, "enabled defaults to true");
        assert_eq!(cfg.poll_interval_secs, DEFAULT_POLL_SECS);
        assert!(!cfg.exclude_dirs.is_empty(), "exclude_dirs has sane defaults");
    }

    #[test]
    fn parses_full_config_with_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obsidian.toml");
        std::fs::write(
            &path,
            r#"
vault_path = "/tmp/vault"
enabled = false
poll_interval_secs = 120
exclude_dirs = [".custom", ".also"]
"#,
        )
        .unwrap();
        let cfg = ObsidianConfig::load_from(&path).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 120);
        assert_eq!(cfg.exclude_dirs, vec![".custom".to_string(), ".also".to_string()]);
    }

    #[test]
    fn missing_vault_path_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obsidian.toml");
        std::fs::write(&path, "enabled = true\n").unwrap();
        let err = ObsidianConfig::load_from(&path).unwrap_err();
        assert!(matches!(err, ObsidianConfigError::Toml { .. }));
    }

    #[test]
    fn nonexistent_file_is_an_io_error() {
        let err = ObsidianConfig::load_from(Path::new("/nope/obsidian.toml")).unwrap_err();
        assert!(matches!(err, ObsidianConfigError::Io { .. }));
    }
}
