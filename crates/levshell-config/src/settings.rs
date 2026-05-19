//! Global daemon configuration: `~/.config/levshell/levshell.toml`.
//!
//! Until now this file was a Phase-0 stub nothing parsed, so the
//! UI-scale / follow-system / density *defaults* could not persist
//! across a daemon restart (only the runtime `levshell-ctl` commands
//! changed them). This module makes the `[shell]` and `[appearance]`
//! sections functional. Everything is optional and fail-soft: a
//! missing or malformed file yields [`Settings::default`] and the
//! daemon boots exactly as before.
//!
//! Runtime `levshell-ctl` commands still win — these values only seed
//! the initial state on shell connect / daemon boot.

use std::path::PathBuf;

use serde::Deserialize;

use crate::profiles::default_config_base;

/// `[shell]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShellSettings {
    /// `"full"` | `"compact"` | `"hidden"`. Validated on load; an
    /// unknown value is dropped (treated as unset).
    #[serde(default)]
    pub density: Option<String>,
}

/// `[appearance]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppearanceSettings {
    /// Internal UI scale multiplier. Clamped to `[0.5, 4.0]` on load
    /// (same bound as `levshell-ctl scale`).
    #[serde(default)]
    pub ui_scale: Option<f64>,
    /// Follow the system (XDG portal) light/dark preference at boot.
    #[serde(default)]
    pub follow_system: Option<bool>,
}

/// Parsed `levshell.toml`. Unknown sections/keys are ignored by serde,
/// so the existing `[power]`/`[ipc]` stub lines stay harmless.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub shell: ShellSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
}

impl Settings {
    /// The validated UI scale, if set and sane.
    pub fn ui_scale(&self) -> Option<f64> {
        self.appearance
            .ui_scale
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.5, 4.0))
    }

    /// The density string, only if it's a recognized value.
    pub fn density(&self) -> Option<&str> {
        match self.shell.density.as_deref() {
            Some(d @ ("full" | "compact" | "hidden")) => Some(d),
            _ => None,
        }
    }

    /// Whether to follow the system light/dark preference at boot.
    pub fn follow_system(&self) -> bool {
        self.appearance.follow_system.unwrap_or(false)
    }
}

/// `$XDG_CONFIG_HOME/levshell/levshell.toml` (or `~/.config/...`).
pub fn default_settings_path() -> Option<PathBuf> {
    Some(default_config_base()?.join("levshell.toml"))
}

/// Load and validate the global settings. Fail-soft: a missing file or
/// any parse error logs and returns [`Settings::default`] — the daemon
/// must always boot.
pub fn load_settings() -> Settings {
    let Some(path) = default_settings_path() else {
        tracing::debug!("no config base dir; using built-in defaults");
        return Settings::default();
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "no levshell.toml; using defaults");
            return Settings::default();
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e,
                "levshell.toml unreadable; using defaults");
            return Settings::default();
        }
    };
    match toml::from_str::<Settings>(&raw) {
        Ok(s) => {
            tracing::info!(
                path = %path.display(),
                ui_scale = ?s.ui_scale(),
                density = ?s.density(),
                follow_system = s.follow_system(),
                "loaded levshell.toml"
            );
            s
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e,
                "levshell.toml parse error; using defaults");
            Settings::default()
        }
    }
}

/// The keys `write_setting` accepts, with their value validation. The
/// surface is deliberately closed — only what the settings UI / config
/// CLI persists — so an arbitrary key can't be written.
fn validated(key: &str, value: &str) -> Result<(&'static str, &'static str, toml_edit::Value), String> {
    match key {
        "appearance.ui_scale" => {
            let v: f64 = value
                .parse()
                .map_err(|_| format!("ui_scale: not a number: {value:?}"))?;
            if !v.is_finite() || !(0.5..=4.0).contains(&v) {
                return Err(format!("ui_scale must be within 0.5..=4.0, got {v}"));
            }
            Ok(("appearance", "ui_scale", v.into()))
        }
        "appearance.follow_system" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("follow_system: not a bool: {value:?}"))?;
            Ok(("appearance", "follow_system", v.into()))
        }
        "shell.density" => match value {
            "full" | "compact" | "hidden" => {
                Ok(("shell", "density", value.into()))
            }
            other => Err(format!(
                "density must be full|compact|hidden, got {other:?}"
            )),
        },
        other => Err(format!(
            "unknown config key {other:?} (writable: appearance.ui_scale, \
             appearance.follow_system, shell.density)"
        )),
    }
}

/// Set one `levshell.toml` key, preserving the user's comments and
/// layout (`toml_edit`). Validates the value, creating the file /
/// section if missing, and writes atomically (tmp + rename) so a
/// crash mid-write can't truncate the config.
pub fn write_setting(path: &std::path::Path, key: &str, value: &str) -> Result<(), String> {
    let (table, leaf, val) = validated(key, value)?;

    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;

    let tbl = doc
        .entry(table)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let tbl = tbl
        .as_table_mut()
        .ok_or_else(|| format!("[{table}] in {} is not a table", path.display()))?;
    tbl.insert(leaf, toml_edit::Item::Value(val));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())
        .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("replacing {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Settings {
        toml::from_str::<Settings>(s).unwrap()
    }

    #[test]
    fn empty_is_all_defaults() {
        let s = Settings::default();
        assert_eq!(s.ui_scale(), None);
        assert_eq!(s.density(), None);
        assert!(!s.follow_system());
    }

    #[test]
    fn full_config_parses_and_validates() {
        let s = parse(
            r#"
[shell]
density = "compact"

[appearance]
ui_scale = 1.75
follow_system = true
"#,
        );
        assert_eq!(s.ui_scale(), Some(1.75));
        assert_eq!(s.density(), Some("compact"));
        assert!(s.follow_system());
    }

    #[test]
    fn ui_scale_is_clamped() {
        assert_eq!(parse("[appearance]\nui_scale = 99.0").ui_scale(), Some(4.0));
        assert_eq!(parse("[appearance]\nui_scale = 0.1").ui_scale(), Some(0.5));
        assert_eq!(parse("[appearance]\nui_scale = 2.0").ui_scale(), Some(2.0));
    }

    #[test]
    fn unknown_density_is_dropped() {
        assert_eq!(parse(r#"[shell]
density = "enormous""#).density(), None);
    }

    #[test]
    fn unknown_sections_are_ignored() {
        // The legacy [power]/[ipc] stub lines must not break parsing.
        let s = parse(
            r#"
[power]
power_aware = true

[ipc]
socket_path = "/tmp/x.sock"

[appearance]
ui_scale = 1.5
"#,
        );
        assert_eq!(s.ui_scale(), Some(1.5));
    }

    #[test]
    fn write_setting_preserves_comments_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("levshell.toml");
        std::fs::write(
            &p,
            "# my hand-written note\n[appearance]\nui_scale = 1.0  # keep me\n",
        )
        .unwrap();

        write_setting(&p, "appearance.ui_scale", "1.75").unwrap();
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("# my hand-written note"), "comment kept: {after}");
        assert!(after.contains("1.75"), "value updated: {after}");
        assert_eq!(load_at(&p).ui_scale(), Some(1.75));

        // New section is created when absent.
        write_setting(&p, "shell.density", "compact").unwrap();
        assert_eq!(load_at(&p).density(), Some("compact"));
        assert!(std::fs::read_to_string(&p).unwrap().contains("# my hand-written note"));
    }

    #[test]
    fn write_setting_creates_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("levshell.toml");
        write_setting(&p, "appearance.follow_system", "true").unwrap();
        assert!(load_at(&p).follow_system());
    }

    #[test]
    fn write_setting_rejects_bad_key_and_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("levshell.toml");
        assert!(write_setting(&p, "appearance.bogus", "1").unwrap_err().contains("unknown config key"));
        assert!(write_setting(&p, "appearance.ui_scale", "99").unwrap_err().contains("0.5..=4.0"));
        assert!(write_setting(&p, "shell.density", "enormous").unwrap_err().contains("full|compact|hidden"));
        // A rejected write must not have created the file.
        assert!(!p.exists(), "no file written on validation failure");
    }

    fn load_at(p: &std::path::Path) -> Settings {
        toml::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }
}
