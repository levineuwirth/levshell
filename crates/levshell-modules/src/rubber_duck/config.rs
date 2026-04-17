//! Rubber-duck debugger configuration (`~/.config/levshell/rubber_duck.toml`).
//!
//! All fields have defaults; the whole file is optional. Spec §2.12.6
//! describes the rubber duck as "a minimal chat interface connected to
//! a local LLM" — we default to Ollama on `localhost:11434` because
//! that's the overwhelmingly common local-LLM setup on Linux.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_ENDPOINT: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "llama3.2:3b";
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const DEFAULT_SYSTEM_PROMPT: &str = "\
You are a rubber-duck debugging partner. The user is stuck on a problem \
and needs to articulate it out loud. Ask short, specific clarifying \
questions that help them externalize what they already know. Do not \
propose solutions unless explicitly asked. Keep replies concise.";

#[derive(Debug, Error)]
pub enum RubberDuckConfigError {
    #[error("reading rubber-duck config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing rubber-duck config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

fn default_enabled() -> bool {
    true
}
fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.to_owned()
}
fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}
fn default_system_prompt() -> String {
    DEFAULT_SYSTEM_PROMPT.to_owned()
}
fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubberDuckConfig {
    /// Master switch. When `false`, ctl / keybind invocations surface
    /// an error instead of opening the overlay. Intended for users
    /// who don't have Ollama installed and don't want the ctl
    /// command to silently hang.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Ollama HTTP endpoint (no trailing slash). `/api/chat` is
    /// appended at request time.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Model tag to send to Ollama. User is responsible for
    /// `ollama pull <model>` before first use.
    #[serde(default = "default_model")]
    pub model: String,

    /// System prompt injected as the first message of every new
    /// conversation. Deliberately terse — the rubber-duck role works
    /// best with minimal framing.
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

    /// HTTP timeout for a complete streaming response. Long by
    /// default since local-LLM chat can take a while on slower
    /// hardware.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for RubberDuckConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            endpoint: default_endpoint(),
            model: default_model(),
            system_prompt: default_system_prompt(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl RubberDuckConfig {
    pub fn load_from(path: &Path) -> Result<Self, RubberDuckConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| RubberDuckConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| RubberDuckConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// Default config file path: `$XDG_CONFIG_HOME/levshell/rubber_duck.toml`.
pub fn default_rubber_duck_config_path() -> Option<PathBuf> {
    levshell_config::default_config_base().map(|d| d.join("rubber_duck.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_ollama_localhost() {
        let c = RubberDuckConfig::default();
        assert!(c.enabled);
        assert_eq!(c.endpoint, "http://localhost:11434");
        assert_eq!(c.model, "llama3.2:3b");
        assert_eq!(c.timeout_secs, 120);
        assert!(c.system_prompt.contains("rubber-duck"));
    }

    #[test]
    fn parses_partial_toml() {
        let src = r#"model = "mistral:7b""#;
        let c: RubberDuckConfig = toml::from_str(src).unwrap();
        assert_eq!(c.model, "mistral:7b");
        assert_eq!(c.endpoint, "http://localhost:11434"); // defaulted
    }

    #[test]
    fn parses_custom_endpoint() {
        let src = r#"
            endpoint = "http://gpubox.local:11434"
            model = "qwen2.5:14b"
            timeout_secs = 300
        "#;
        let c: RubberDuckConfig = toml::from_str(src).unwrap();
        assert_eq!(c.endpoint, "http://gpubox.local:11434");
        assert_eq!(c.timeout_secs, 300);
    }

    #[test]
    fn load_from_missing_file_errors_with_path() {
        let err = RubberDuckConfig::load_from(Path::new("/tmp/nonexistent_duck_zzz.toml"))
            .expect_err("should fail");
        match err {
            RubberDuckConfigError::Io { path, .. } => {
                assert_eq!(path, PathBuf::from("/tmp/nonexistent_duck_zzz.toml"));
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn load_from_malformed_file_is_toml_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "not valid = = = toml").unwrap();
        let err = RubberDuckConfig::load_from(&path).expect_err("should fail");
        assert!(matches!(err, RubberDuckConfigError::Toml { .. }));
    }
}
