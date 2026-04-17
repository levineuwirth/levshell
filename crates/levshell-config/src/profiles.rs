//! Profile TOML loader.
//!
//! A profile file looks like:
//!
//! ```toml
//! # ~/.config/levshell/profiles/writing.toml
//! name = "writing"
//! suppress_notifications = true
//!
//! [overrides]
//! clock = "visible"
//! cpu = "badge"
//! notifications = "hidden"
//! ```
//!
//! The file stem doubles as a fallback name: if `name` is omitted, the
//! profile is named after the filename (without extension). Each value in
//! `[overrides]` is a [`Prominence`] serialized in the same snake_case form
//! the IPC layer uses on the wire.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use levshell_context::{parse_expression, AutoTrigger, Profile, Prominence};

/// Errors the loader can emit. Parse errors for a single file are returned
/// here but [`load_profiles_from_dir`] logs and skips them rather than
/// propagating — the daemon should always boot with whatever parsed cleanly.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("reading profile file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing profile file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid auto_trigger expression in profile file {path}: {source}")]
    Trigger {
        path: PathBuf,
        #[source]
        source: levshell_context::ContextError,
    },
}

/// Deserialized shape of one profile TOML file. Converts into a
/// [`Profile`] via [`Self::into_profile`], which is the only thing the
/// context engine cares about.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileFile {
    /// Profile name. Falls back to the file stem when loaded via
    /// [`load_profile_file`] if omitted.
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub suppress_notifications: bool,

    /// Widget → prominence overrides. Keys are widget ids; values are one of
    /// `hidden`, `badge`, `icon_only`, `compact`, `visible`, `expanded`.
    #[serde(default)]
    pub overrides: HashMap<String, Prominence>,

    /// Optional auto-activation trigger. See [`AutoTriggerFile`].
    #[serde(default)]
    pub auto_trigger: Option<AutoTriggerFile>,
}

/// TOML shape for an auto-activation trigger. The `when` field is a
/// predicate in the context-engine expression DSL (same grammar used by
/// relevance rules). It is parsed once at load time; parse failures surface
/// as [`ConfigError::Trigger`].
///
/// Examples:
///
/// ```toml
/// [auto_trigger]
/// when = 'focused.app_id == "org.zotero.Zotero" or focused.title contains "arxiv"'
/// dwell_secs = 30
/// exit_dwell_secs = 60
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct AutoTriggerFile {
    pub when: String,
    /// Sustained-true seconds before activating. Defaults to 30s — long
    /// enough that alt-tab flicker doesn't trigger the profile, short
    /// enough that a deliberate switch feels responsive.
    #[serde(default = "default_dwell_secs")]
    pub dwell_secs: u64,
    /// Sustained-false seconds before deactivating. Defaults to 60s —
    /// roughly twice `dwell_secs` so brief context breaks (checking a
    /// slack ping, glancing at mail) don't kick the user out.
    #[serde(default = "default_exit_dwell_secs")]
    pub exit_dwell_secs: u64,
}

fn default_dwell_secs() -> u64 {
    30
}

fn default_exit_dwell_secs() -> u64 {
    60
}

impl ProfileFile {
    /// Produce a [`Profile`] from this file, using `fallback_name` when
    /// `self.name` is `None`. Parses any `[auto_trigger]` DSL string;
    /// returns [`ConfigError::Trigger`] on parse failure with the given
    /// path attached for diagnostics.
    pub fn into_profile(self, fallback_name: &str, path: &Path) -> Result<Profile, ConfigError> {
        let auto_trigger = match self.auto_trigger {
            None => None,
            Some(t) => Some(AutoTrigger {
                when: parse_expression(&t.when).map_err(|e| ConfigError::Trigger {
                    path: path.to_path_buf(),
                    source: e,
                })?,
                dwell: Duration::from_secs(t.dwell_secs),
                exit_dwell: Duration::from_secs(t.exit_dwell_secs),
            }),
        };
        Ok(Profile {
            name: self.name.unwrap_or_else(|| fallback_name.to_owned()),
            overrides: self.overrides,
            suppress_notifications: self.suppress_notifications,
            auto_trigger,
        })
    }
}

/// Load a single profile file, resolving the fallback name from its stem.
pub fn load_profile_file(path: &Path) -> Result<Profile, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let file: ProfileFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    let fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed");
    file.into_profile(fallback, path)
}

/// Scan a directory for `*.toml` files and load each as a profile.
///
/// Files that fail to parse are logged at `warn` level and skipped so the
/// daemon can still boot with the subset of profiles that loaded cleanly.
/// Non-`*.toml` entries are ignored. The returned vector is sorted by
/// profile name for determinism.
pub fn load_profiles_from_dir(dir: &Path) -> Vec<Profile> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "levshell-config: failed to read profiles directory"
                );
            }
            return Vec::new();
        }
    };

    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_profile_file(&path) {
            Ok(profile) => profiles.push(profile),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "levshell-config: skipping malformed profile"
                );
            }
        }
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    profiles
}

/// Default profiles directory: `$XDG_CONFIG_HOME/levshell/profiles` or
/// `~/.config/levshell/profiles`. Returns `None` if neither env var is set.
pub fn default_profiles_dir() -> Option<PathBuf> {
    levshell_config_base().map(|b| b.join("profiles"))
}

/// Watch `dir` for profile TOML changes and write the full reloaded
/// set into `shared` whenever any file in the directory changes.
///
/// The replacement strategy is simple and correct: on *any* event,
/// reload the whole directory and atomically swap the vec inside the
/// lock. Per-file tracking would let us react more efficiently to a
/// single-file change, but the consumer (the context engine) only
/// reads the vec during resolves, and a full reload of ~10 profile
/// files costs ~100µs — well within reasonable hot-reload budgets.
///
/// Returns a [`ProfileWatcher`] that owns the OS watch handle plus the
/// background reload task. Dropping it stops both; explicit
/// [`ProfileWatcher::shutdown`] waits for the task to exit cleanly.
pub fn spawn_profile_watcher(
    dir: &Path,
    shared: std::sync::Arc<std::sync::RwLock<Vec<Profile>>>,
) -> Result<ProfileWatcher, crate::WatcherError> {
    use crate::watcher::watch_config_dir;

    let (watcher, mut rx) = watch_config_dir(dir)?;
    let dir_owned = dir.to_path_buf();
    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Drain any backlogged events so rapid-fire writes only
            // cause one reload.
            while rx.try_recv().is_ok() {}
            let reloaded = load_profiles_from_dir(&dir_owned);
            let count = reloaded.len();
            match shared.write() {
                Ok(mut guard) => {
                    *guard = reloaded;
                    tracing::info!(
                        dir = %dir_owned.display(),
                        count,
                        "profile hot-reload: new set applied"
                    );
                }
                Err(_) => {
                    tracing::error!(
                        "profile hot-reload: shared lock poisoned; giving up"
                    );
                    return;
                }
            }
        }
        tracing::debug!("profile hot-reload: watcher channel closed");
    });
    Ok(ProfileWatcher {
        _watcher: watcher,
        task,
    })
}

/// Handle to the profiles directory watcher. Owns the OS watch and the
/// background reload task. Drop to stop the watch; call
/// [`Self::shutdown`] to additionally await the task's exit.
pub struct ProfileWatcher {
    _watcher: crate::watcher::ConfigWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl ProfileWatcher {
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

impl std::fmt::Debug for ProfileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileWatcher").finish_non_exhaustive()
    }
}

/// Default sync-adapter configuration directory:
/// `$XDG_CONFIG_HOME/levshell/sync` or `~/.config/levshell/sync`.
/// Each adapter (Obsidian, Zotero, Anki, CalDAV, …) has its own file
/// under this directory. Returns `None` if neither env var is set.
pub fn default_sync_dir() -> Option<PathBuf> {
    levshell_config_base().map(|b| b.join("sync"))
}

/// Base configuration directory: `$XDG_CONFIG_HOME/levshell` or
/// `~/.config/levshell`. Returns `None` if neither env var is set.
/// Use this for single-file configs that don't live under `sync/`,
/// `profiles/`, or `projects/` (e.g. `ideation.toml`).
pub fn default_config_base() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("levshell"))
}

fn levshell_config_base() -> Option<PathBuf> {
    default_config_base()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn parses_well_formed_profile_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "writing.toml",
            r#"
name = "writing"
suppress_notifications = true

[overrides]
clock = "visible"
cpu = "badge"
notifications = "hidden"
"#,
        );
        let profile = load_profile_file(&dir.path().join("writing.toml")).unwrap();
        assert_eq!(profile.name, "writing");
        assert!(profile.suppress_notifications);
        assert_eq!(profile.overrides.get("clock"), Some(&Prominence::Visible));
        assert_eq!(profile.overrides.get("cpu"), Some(&Prominence::Badge));
        assert_eq!(
            profile.overrides.get("notifications"),
            Some(&Prominence::Hidden)
        );
    }

    #[test]
    fn file_stem_is_used_when_name_field_missing() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "focus.toml",
            r#"
[overrides]
cpu = "compact"
"#,
        );
        let profile = load_profile_file(&dir.path().join("focus.toml")).unwrap();
        assert_eq!(profile.name, "focus");
        assert!(!profile.suppress_notifications);
    }

    #[test]
    fn load_dir_returns_all_valid_profiles_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "zzz.toml",
            r#"name = "zulu"
[overrides]
clock = "expanded"
"#,
        );
        write(
            dir.path(),
            "aaa.toml",
            r#"name = "alpha"
[overrides]
clock = "compact"
"#,
        );
        let profiles = load_profiles_from_dir(dir.path());
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "alpha");
        assert_eq!(profiles[1].name, "zulu");
    }

    #[test]
    fn load_dir_skips_malformed_files_and_returns_valid_ones() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "broken.toml",
            "this is not valid = = toml {{{",
        );
        write(
            dir.path(),
            "good.toml",
            r#"name = "good"
[overrides]
clock = "visible"
"#,
        );
        let profiles = load_profiles_from_dir(dir.path());
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "good");
    }

    #[test]
    fn load_dir_ignores_non_toml_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "not a profile");
        write(
            dir.path(),
            "valid.toml",
            r#"name = "valid"
"#,
        );
        let profiles = load_profiles_from_dir(dir.path());
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "valid");
    }

    #[test]
    fn load_dir_returns_empty_when_directory_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(load_profiles_from_dir(&missing).is_empty());
    }

    #[test]
    fn parses_auto_trigger_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "lit-review.toml",
            r#"
[auto_trigger]
when = 'focused.app_id == "org.zotero.Zotero"'
"#,
        );
        let profile = load_profile_file(&dir.path().join("lit-review.toml")).unwrap();
        let t = profile.auto_trigger.as_ref().expect("auto_trigger set");
        assert_eq!(t.dwell, Duration::from_secs(30));
        assert_eq!(t.exit_dwell, Duration::from_secs(60));
    }

    #[test]
    fn parses_auto_trigger_with_custom_dwell_times() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "writing.toml",
            r#"
[auto_trigger]
when = 'focused.title contains ".tex"'
dwell_secs = 10
exit_dwell_secs = 120
"#,
        );
        let profile = load_profile_file(&dir.path().join("writing.toml")).unwrap();
        let t = profile.auto_trigger.as_ref().expect("auto_trigger set");
        assert_eq!(t.dwell, Duration::from_secs(10));
        assert_eq!(t.exit_dwell, Duration::from_secs(120));
    }

    #[test]
    fn invalid_trigger_expression_surfaces_trigger_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "broken.toml",
            r#"
[auto_trigger]
when = 'focused.app_id =='
"#,
        );
        let result = load_profile_file(&dir.path().join("broken.toml"));
        assert!(matches!(result, Err(ConfigError::Trigger { .. })));
    }

    #[test]
    fn shipped_lit_review_example_parses() {
        // Confidence check: the repo's config/profiles/lit-review.toml
        // stays valid as the DSL evolves. The include_str! path is
        // relative to *this* source file.
        let body = include_str!("../../../config/profiles/lit-review.toml");
        let parsed: ProfileFile = toml::from_str(body).expect("valid toml");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lit-review.toml");
        std::fs::write(&path, body).unwrap();
        let profile = parsed.into_profile("lit-review", &path).unwrap();
        assert_eq!(profile.name, "lit-review");
        assert!(profile.auto_trigger.is_some());
        assert!(profile.suppress_notifications);
    }

    #[test]
    fn shipped_writing_example_parses() {
        let body = include_str!("../../../config/profiles/writing.toml");
        let parsed: ProfileFile = toml::from_str(body).expect("valid toml");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writing.toml");
        std::fs::write(&path, body).unwrap();
        let profile = parsed.into_profile("writing", &path).unwrap();
        assert_eq!(profile.name, "writing");
        assert!(profile.auto_trigger.is_some());
        assert!(profile.suppress_notifications);
    }

    #[test]
    fn profile_without_trigger_has_none() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "manual.toml",
            r#"
[overrides]
cpu = "badge"
"#,
        );
        let profile = load_profile_file(&dir.path().join("manual.toml")).unwrap();
        assert!(profile.auto_trigger.is_none());
    }

    #[test]
    fn unknown_prominence_value_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bad.toml",
            r#"
[overrides]
clock = "enormous"
"#,
        );
        let result = load_profile_file(&dir.path().join("bad.toml"));
        assert!(matches!(result, Err(ConfigError::Parse { .. })));
    }
}
