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

use serde::Deserialize;
use thiserror::Error;

use levshell_context::{Profile, Prominence};

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
}

impl ProfileFile {
    /// Produce a [`Profile`] from this file, using `fallback_name` when
    /// `self.name` is `None`.
    pub fn into_profile(self, fallback_name: &str) -> Profile {
        Profile {
            name: self.name.unwrap_or_else(|| fallback_name.to_owned()),
            overrides: self.overrides,
            suppress_notifications: self.suppress_notifications,
        }
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
    Ok(file.into_profile(fallback))
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
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("levshell").join("profiles"))
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
