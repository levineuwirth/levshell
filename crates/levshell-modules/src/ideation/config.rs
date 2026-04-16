//! Ideation engine configuration (`~/.config/levshell/ideation.toml`).
//!
//! All fields have sensible defaults — the file is optional. Spec §2.2
//! prescribes a Poisson-distributed nudge cadence around a mean of ~45
//! minutes; we discretize that via a fixed tick and a per-tick Bernoulli
//! trial (`p = tick / lambda`) which approximates Poisson inter-arrivals
//! for `tick ≪ lambda`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_LAMBDA_MINUTES: f64 = 45.0;
const DEFAULT_TICK_SECS: u64 = 60;
const DEFAULT_BLOCKED_ESCALATION_FACTOR: f64 = 3.0;
const DEFAULT_STALE_PROJECT_HOURS: u64 = 24;
const DEFAULT_RECENT_SEED_HOURS: u64 = 6;

#[derive(Debug, Error)]
pub enum IdeationConfigError {
    #[error("reading ideation config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing ideation config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_enabled() -> bool {
    true
}
fn default_lambda_minutes() -> f64 {
    DEFAULT_LAMBDA_MINUTES
}
fn default_tick_secs() -> u64 {
    DEFAULT_TICK_SECS
}
fn default_blocked_factor() -> f64 {
    DEFAULT_BLOCKED_ESCALATION_FACTOR
}
fn default_stale_hours() -> u64 {
    DEFAULT_STALE_PROJECT_HOURS
}
fn default_recent_hours() -> u64 {
    DEFAULT_RECENT_SEED_HOURS
}
fn default_weights() -> NudgeWeights {
    NudgeWeights::default()
}

/// Relative weights for the three nudge kinds. Normalized at
/// selection time — absolute magnitudes don't matter. A weight of
/// zero disables that category entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeWeights {
    #[serde(default = "default_open_question_weight")]
    pub open_question: f64,
    #[serde(default = "default_cross_connection_weight")]
    pub cross_connection: f64,
    #[serde(default = "default_blocked_weight")]
    pub blocked: f64,
}

fn default_open_question_weight() -> f64 {
    0.7
}
fn default_cross_connection_weight() -> f64 {
    0.2
}
fn default_blocked_weight() -> f64 {
    0.1
}

impl Default for NudgeWeights {
    fn default() -> Self {
        Self {
            open_question: default_open_question_weight(),
            cross_connection: default_cross_connection_weight(),
            blocked: default_blocked_weight(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeationConfig {
    /// Whether the engine runs. When `false` the module starts but
    /// every tick is a no-op (no probe, no selection, no publish).
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Mean inter-nudge interval in minutes. Spec §2.2 recommends 45.
    /// Accepts fractional values so tests can set tiny means.
    #[serde(default = "default_lambda_minutes")]
    pub lambda_minutes: f64,

    /// How often the module wakes up to roll the Bernoulli trial. The
    /// effective nudge rate is `tick / (lambda * 60)` per tick; the
    /// value must divide lambda finely enough for the approximation
    /// to hold (`tick ≤ lambda/10` is the rule of thumb).
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,

    /// Per-kind selection weights. Normalized at selection time.
    #[serde(default = "default_weights")]
    pub weights: NudgeWeights,

    /// Multiplier applied to the per-tick fire probability when at
    /// least one project is in the `Blocked` status. Spec §2.4 calls
    /// for increased nudge frequency on blocked projects; 3× is the
    /// v1 default.
    #[serde(default = "default_blocked_factor")]
    pub blocked_escalation_factor: f64,

    /// A project counts as "stale" (and therefore eligible for
    /// escalation even without the `Blocked` status) once its
    /// `updated_at` is this many hours in the past.
    #[serde(default = "default_stale_hours")]
    pub stale_project_hours: u64,

    /// How far back we look when picking a "recently synced" entity
    /// for the cross-connection seed. Entities outside this window
    /// are ignored.
    #[serde(default = "default_recent_hours")]
    pub recent_seed_hours: u64,
}

impl Default for IdeationConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            lambda_minutes: default_lambda_minutes(),
            tick_secs: default_tick_secs(),
            weights: NudgeWeights::default(),
            blocked_escalation_factor: default_blocked_factor(),
            stale_project_hours: default_stale_hours(),
            recent_seed_hours: default_recent_hours(),
        }
    }
}

impl IdeationConfig {
    pub fn tick_interval(&self) -> Duration {
        Duration::from_secs(self.tick_secs.max(1))
    }

    /// Per-tick probability of firing a nudge in the unescalated case.
    /// Derived from `tick / (lambda * 60)`. Clamped to `[0.0, 1.0]`
    /// so a misconfiguration (tick longer than lambda) doesn't
    /// underflow or over-fire.
    pub fn base_fire_probability(&self) -> f64 {
        if self.lambda_minutes <= 0.0 {
            return 1.0;
        }
        let p = self.tick_secs as f64 / (self.lambda_minutes * 60.0);
        p.clamp(0.0, 1.0)
    }

    pub fn load_from(path: &Path) -> Result<Self, IdeationConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| IdeationConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| IdeationConfigError::Toml {
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
        let cfg = IdeationConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.lambda_minutes, 45.0);
        assert_eq!(cfg.tick_secs, 60);
        assert_eq!(cfg.weights.open_question, 0.7);
    }

    #[test]
    fn base_fire_probability_is_tick_over_lambda() {
        let cfg = IdeationConfig {
            tick_secs: 60,
            lambda_minutes: 30.0,
            ..IdeationConfig::default()
        };
        // 60 / (30 * 60) = 1/30
        assert!((cfg.base_fire_probability() - 1.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn base_fire_probability_clamps_when_tick_exceeds_lambda() {
        let cfg = IdeationConfig {
            tick_secs: 3600,
            lambda_minutes: 30.0,
            ..IdeationConfig::default()
        };
        assert_eq!(cfg.base_fire_probability(), 1.0);
    }

    #[test]
    fn parses_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ideation.toml");
        std::fs::write(&path, "").unwrap();
        let cfg = IdeationConfig::load_from(&path).unwrap();
        assert_eq!(cfg.lambda_minutes, 45.0);
    }

    #[test]
    fn parses_partial_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ideation.toml");
        std::fs::write(
            &path,
            r#"
lambda_minutes = 15.0
blocked_escalation_factor = 5.0
[weights]
open_question = 0.5
cross_connection = 0.5
blocked = 0.0
"#,
        )
        .unwrap();
        let cfg = IdeationConfig::load_from(&path).unwrap();
        assert_eq!(cfg.lambda_minutes, 15.0);
        assert_eq!(cfg.blocked_escalation_factor, 5.0);
        assert_eq!(cfg.weights.open_question, 0.5);
        assert_eq!(cfg.weights.blocked, 0.0);
        assert_eq!(cfg.tick_secs, 60, "defaults preserved for unspecified");
    }
}
