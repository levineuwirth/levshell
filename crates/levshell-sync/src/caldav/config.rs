//! CalDAV adapter configuration
//! (`~/.config/levshell/sync/caldav.toml`).
//!
//! One TOML file declares multiple calendars under `[[calendar]]`
//! blocks. Each calendar has its own URL (a collection, not the
//! principal root) and credentials. v1 only supports HTTP Basic auth
//! — `username` + either inline `password` or `password_command`
//! (executed at load time to let users source credentials from
//! `pass`, `gopass`, a keyring script, etc.).
//!
//! ```toml
//! enabled = true
//! poll_interval_secs = 600
//!
//! [[calendar]]
//! name = "work"
//! url = "https://cloud.example/remote.php/dav/calendars/u/work/"
//! username = "u"
//! password_command = "pass caldav/cloud"
//! # or:
//! # password = "plaintext"
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_POLL_SECS: u64 = 600;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum CalDavConfigError {
    #[error("reading caldav config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing caldav config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("running password_command for calendar {calendar}: {source}")]
    PasswordCommandIo {
        calendar: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "password_command for calendar {calendar} exited with {status}: {stderr}"
    )]
    PasswordCommandExit {
        calendar: String,
        status: String,
        stderr: String,
    },

    #[error("calendar {calendar} must set either password or password_command")]
    MissingPassword { calendar: String },
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

/// Wire form of the TOML file. `calendar` is a TOML array of tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// How often to PROPFIND each calendar. Default 10 min matches
    /// what most personal calendar clients do.
    #[serde(default = "default_poll_secs")]
    pub poll_interval_secs: u64,

    /// Per-request HTTP timeout.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// Calendar collections to sync. Empty → adapter runs but syncs
    /// nothing (useful while the user is setting up credentials).
    #[serde(default, rename = "calendar")]
    pub calendars: Vec<CalendarSource>,
}

impl Default for CalDavConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            poll_interval_secs: default_poll_secs(),
            request_timeout_secs: default_request_timeout_secs(),
            calendars: Vec::new(),
        }
    }
}

impl CalDavConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs.max(1))
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    pub fn load_from(path: &Path) -> Result<Self, CalDavConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| CalDavConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| CalDavConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarSource {
    /// Stable short name. Used as the first segment of the
    /// sync_metadata external_id so two calendars with the same UID
    /// (rare but possible across servers) don't collide.
    pub name: String,

    /// Calendar collection URL — the one that responds to PROPFIND
    /// with a list of .ics hrefs. Principal URLs and WebDAV roots
    /// work too but are slower (one extra redirect).
    pub url: String,

    pub username: String,

    /// Inline password. Convenient for testing; prefer
    /// `password_command` in real configs so plaintext doesn't live
    /// on disk.
    #[serde(default)]
    pub password: Option<String>,

    /// Shell command whose stdout is used as the password. Executed
    /// once at config-load time via /bin/sh -c. Trailing newline is
    /// stripped. Shell-quoting is the user's responsibility — this is
    /// the same contract git's `credential.helper = !` uses.
    #[serde(default)]
    pub password_command: Option<String>,
}

impl CalendarSource {
    /// Resolve the password, running `password_command` if set and
    /// falling back to `password`. Fails if neither is set.
    pub fn resolve_password(&self) -> Result<String, CalDavConfigError> {
        if let Some(cmd) = &self.password_command {
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .map_err(|e| CalDavConfigError::PasswordCommandIo {
                    calendar: self.name.clone(),
                    source: e,
                })?;
            if !output.status.success() {
                return Err(CalDavConfigError::PasswordCommandExit {
                    calendar: self.name.clone(),
                    status: output.status.to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            let raw = String::from_utf8_lossy(&output.stdout);
            // Strip a single trailing newline (common from `pass`,
            // `echo`, etc.) but preserve embedded newlines — some
            // credential helpers emit structured output.
            let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
            return Ok(trimmed.to_string());
        }
        self.password
            .clone()
            .ok_or_else(|| CalDavConfigError::MissingPassword {
                calendar: self.name.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let cfg = CalDavConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 600);
        assert!(cfg.calendars.is_empty());
    }

    #[test]
    fn parses_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caldav.toml");
        std::fs::write(&path, "").unwrap();
        let cfg = CalDavConfig::load_from(&path).unwrap();
        assert!(cfg.calendars.is_empty());
    }

    #[test]
    fn parses_one_calendar_with_inline_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caldav.toml");
        std::fs::write(
            &path,
            r#"
poll_interval_secs = 120

[[calendar]]
name = "work"
url = "https://example.org/cal/work/"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        let cfg = CalDavConfig::load_from(&path).unwrap();
        assert_eq!(cfg.poll_interval_secs, 120);
        assert_eq!(cfg.calendars.len(), 1);
        let c = &cfg.calendars[0];
        assert_eq!(c.name, "work");
        assert_eq!(c.url, "https://example.org/cal/work/");
        assert_eq!(c.username, "u");
        assert_eq!(c.resolve_password().unwrap(), "p");
    }

    #[test]
    fn parses_multiple_calendars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caldav.toml");
        std::fs::write(
            &path,
            r#"
[[calendar]]
name = "work"
url = "https://a"
username = "u"
password = "p"

[[calendar]]
name = "personal"
url = "https://b"
username = "u"
password = "p"
"#,
        )
        .unwrap();
        let cfg = CalDavConfig::load_from(&path).unwrap();
        assert_eq!(cfg.calendars.len(), 2);
        assert_eq!(cfg.calendars[1].name, "personal");
    }

    #[test]
    fn password_command_strips_trailing_newline() {
        let c = CalendarSource {
            name: "t".into(),
            url: "x".into(),
            username: "u".into(),
            password: None,
            password_command: Some("printf 'hunter2\\n'".into()),
        };
        assert_eq!(c.resolve_password().unwrap(), "hunter2");
    }

    #[test]
    fn password_command_failure_surfaces_exit_code() {
        let c = CalendarSource {
            name: "t".into(),
            url: "x".into(),
            username: "u".into(),
            password: None,
            password_command: Some("false".into()),
        };
        let err = c.resolve_password().unwrap_err();
        assert!(matches!(err, CalDavConfigError::PasswordCommandExit { .. }));
    }

    #[test]
    fn missing_both_password_fields_errors() {
        let c = CalendarSource {
            name: "t".into(),
            url: "x".into(),
            username: "u".into(),
            password: None,
            password_command: None,
        };
        let err = c.resolve_password().unwrap_err();
        assert!(matches!(err, CalDavConfigError::MissingPassword { .. }));
    }
}
