//! Warmup-mode configuration (`~/.config/levshell/warmup.toml`).
//!
//! All fields default; the file is optional. Spec §2.12.1 says warmup
//! fires "on session start (first unlock of the day or after long
//! idle)" — we approximate that with a gap heuristic, and leave the
//! calendar-day trigger as an opt-in user preference.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_GAP_SECS: u64 = 4 * 60 * 60;

#[derive(Debug, Error)]
pub enum WarmupConfigError {
    #[error("reading warmup config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing warmup config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_enabled() -> bool {
    true
}
fn default_gap_secs() -> u64 {
    DEFAULT_GAP_SECS
}
fn default_calendar_day_trigger() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupConfig {
    /// Master switch. When `false`, the module starts but never fires.
    /// `ctl warmup open` still works — explicit invocation bypasses the
    /// gate.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Required gap (in seconds) between the last observed sway event
    /// and the current one for a fire to qualify, AND between the
    /// current fire and the previously stamped `last_warmup_at`.
    /// Defaults to 4h.
    #[serde(default = "default_gap_secs")]
    pub gap_secs: u64,

    /// Opt-in: fire at the first activity of a new calendar day even
    /// if the gap is below `gap_secs`. Most users won't want this
    /// (continuous overnight work shouldn't interrupt at 00:00); off
    /// by default.
    #[serde(default = "default_calendar_day_trigger")]
    pub calendar_day_trigger: bool,
}

impl Default for WarmupConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            gap_secs: default_gap_secs(),
            calendar_day_trigger: default_calendar_day_trigger(),
        }
    }
}

impl WarmupConfig {
    pub fn gap(&self) -> Duration {
        Duration::from_secs(self.gap_secs.max(1))
    }

    pub fn load_from(path: &Path) -> Result<Self, WarmupConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| WarmupConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| WarmupConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// Default config file path: `$XDG_CONFIG_HOME/levshell/warmup.toml`.
pub fn default_warmup_config_path() -> Option<PathBuf> {
    levshell_config::default_config_base().map(|d| d.join("warmup.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_discussed_contract() {
        let cfg = WarmupConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.gap_secs, 14_400);
        assert!(!cfg.calendar_day_trigger);
    }

    #[test]
    fn parses_partial_toml() {
        let src = r#"gap_secs = 1800"#;
        let cfg: WarmupConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.gap_secs, 1800);
        assert!(cfg.enabled); // defaulted
        assert!(!cfg.calendar_day_trigger);
    }

    #[test]
    fn load_from_missing_file_errors_with_path() {
        let err = WarmupConfig::load_from(Path::new("/tmp/nonexistent_warmup_zzz.toml"))
            .expect_err("should fail");
        match err {
            WarmupConfigError::Io { path, .. } => {
                assert_eq!(path, PathBuf::from("/tmp/nonexistent_warmup_zzz.toml"))
            }
            _ => panic!("expected Io, got {err:?}"),
        }
    }
}
