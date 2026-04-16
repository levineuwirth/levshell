//! Obsidian adapter configuration (`~/.config/levshell/sync/obsidian.toml`).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_POLL_SECS: u64 = 60;

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
}
