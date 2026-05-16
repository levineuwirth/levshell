//! Theme file loader (spec design doc §11 "Theming Architecture").
//!
//! Themes live in `~/.config/levshell/themes/*.toml`. Each file is one
//! theme — the filename stem (`warm-dark.toml` → `"warm-dark"`) is
//! the canonical identifier used by `levshell-ctl theme set`.
//!
//! The format is **partial-override**: every section except `[meta]`
//! and `[colors]` is optional, and within the override sections every
//! field is `Option<String>`. Tokens the user doesn't supply fall
//! back to the Theme.qml built-in defaults. This lets minimal
//! community themes supply just a palette and inherit the rest.
//!
//! The parser returns string color values verbatim — hex validation
//! happens at the QML boundary. Parsing is lossy for unknown sections
//! (`#[serde(default)]`) so spec extensions don't break older daemons.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ThemeFileError {
    #[error("reading theme file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing theme file {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("theme {name} not found in {dir}")]
    NotFound { name: String, dir: PathBuf },

    #[error("theme file {path} has invalid variant {variant:?} (expected \"dark\" or \"light\")")]
    BadVariant { path: PathBuf, variant: String },
}

/// Wire form of a `<name>.toml` file. `meta` and `colors` are
/// required; every other section is optional, and within each
/// optional section every field is itself optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeFile {
    pub meta: ThemeMeta,
    pub colors: ColorTokens,

    #[serde(default)]
    pub health: Option<HealthTokens>,
    #[serde(default)]
    pub bar: Option<BarTokens>,
    #[serde(default)]
    pub typography: Option<TypographyTokens>,
    #[serde(default)]
    pub icons: Option<IconTokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMeta {
    pub name: String,
    #[serde(default)]
    pub author: Option<String>,
    /// `"dark"` or `"light"`. Validated at load time.
    pub variant: String,
    /// Optional name of the paired theme for mode-toggling. The
    /// daemon resolves this against the themes dir at toggle time.
    #[serde(default)]
    pub light_pair: Option<String>,
    #[serde(default)]
    pub dark_pair: Option<String>,
}

/// Flat color palette keyed by the dot-notation names from the spec
/// (`bg.dark`, `surface.raised`, `on.primary`, etc.). Serde's
/// `rename` attributes keep the spec notation on the wire — TOML
/// accepts dotted keys natively when written as `bg.dark = "#..."`,
/// which TOML collapses into a nested table. We model as flat fields
/// and use `[colors.bg]`-style section-free syntax in the TOML file
/// to keep a single flat `[colors]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColorTokens {
    // --- 4-tier surface hierarchy ---
    #[serde(default)]
    pub bg: Option<String>,
    #[serde(default, rename = "bg_dark")]
    pub bg_dark: Option<String>,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default, rename = "surface_raised")]
    pub surface_raised: Option<String>,
    #[serde(default)]
    pub overlay: Option<String>,

    // --- Content colors ---
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default, rename = "fg_muted")]
    pub fg_muted: Option<String>,
    #[serde(default, rename = "fg_subtle")]
    pub fg_subtle: Option<String>,
    #[serde(default, rename = "on_primary")]
    pub on_primary: Option<String>,
    #[serde(default, rename = "on_surface")]
    pub on_surface: Option<String>,
    #[serde(default)]
    pub outline: Option<String>,

    // --- Accents ---
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default, rename = "primary_variant")]
    pub primary_variant: Option<String>,
    #[serde(default)]
    pub secondary: Option<String>,
    #[serde(default, rename = "secondary_variant")]
    pub secondary_variant: Option<String>,
    #[serde(default)]
    pub tertiary: Option<String>,

    // --- State ---
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub info: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthTokens {
    #[serde(default, rename = "stale_pill")]
    pub stale_pill: Option<String>,
    #[serde(default, rename = "error_pill")]
    pub error_pill: Option<String>,
}

/// `[bar]` section. Controls adaptive blur + opacity and the two
/// density-specific heights.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BarTokens {
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default, rename = "blur_radius")]
    pub blur_radius: Option<u32>,
    #[serde(default, rename = "opacity_battery")]
    pub opacity_battery: Option<f64>,
    #[serde(default, rename = "blur_radius_battery")]
    pub blur_radius_battery: Option<u32>,
    #[serde(default, rename = "height_full")]
    pub height_full: Option<u32>,
    #[serde(default, rename = "height_compact")]
    pub height_compact: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypographyTokens {
    #[serde(default, rename = "font_text")]
    pub font_text: Option<String>,
    #[serde(default, rename = "font_mono")]
    pub font_mono: Option<String>,
    #[serde(default, rename = "font_icon")]
    pub font_icon: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IconTokens {
    /// `"outlined"` or `"duotone"`. Stringly-typed because Theme.qml
    /// is the consumer and it already is.
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default, rename = "duotone_secondary")]
    pub duotone_secondary: Option<String>,
}

impl ThemeFile {
    pub fn load_from(path: &Path) -> Result<Self, ThemeFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| ThemeFileError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let parsed: Self = toml::from_str(&text).map_err(|source| ThemeFileError::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        if !matches!(parsed.meta.variant.as_str(), "dark" | "light") {
            return Err(ThemeFileError::BadVariant {
                path: path.to_path_buf(),
                variant: parsed.meta.variant.clone(),
            });
        }
        Ok(parsed)
    }
}

/// Default themes directory (`$XDG_CONFIG_HOME/levshell/themes`).
pub fn default_themes_dir() -> Option<PathBuf> {
    crate::profiles::default_config_base().map(|b| b.join("themes"))
}

/// Bundled theme files, embedded at compile time. `bootstrap_themes`
/// writes these into the user config dir on first run so the daemon
/// has something to load without a manual `cp`.
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    (
        "warm-dark",
        include_str!("../../../config/themes/warm-dark.toml"),
    ),
    (
        "neutral-dark",
        include_str!("../../../config/themes/neutral-dark.toml"),
    ),
    (
        "warm-light",
        include_str!("../../../config/themes/warm-light.toml"),
    ),
];

#[derive(Debug, Clone)]
pub struct BootstrapReport {
    pub dir: PathBuf,
    pub written: Vec<String>,
    pub skipped: Vec<String>,
}

/// Write each [`BUILTIN_THEMES`] entry to `<dir>/<name>.toml`, creating
/// `dir` if needed. Existing files are skipped unless `force` is true.
pub fn bootstrap_themes(dir: &Path, force: bool) -> std::io::Result<BootstrapReport> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for (name, body) in BUILTIN_THEMES {
        let path = dir.join(format!("{name}.toml"));
        if path.exists() && !force {
            skipped.push((*name).to_string());
            continue;
        }
        std::fs::write(&path, body)?;
        written.push((*name).to_string());
    }
    Ok(BootstrapReport {
        dir: dir.to_path_buf(),
        written,
        skipped,
    })
}

/// Load a theme by name from a directory. The filename is
/// `<name>.toml`; unknown names return `NotFound`.
pub fn load_theme(dir: &Path, name: &str) -> Result<ThemeFile, ThemeFileError> {
    let path = dir.join(format!("{name}.toml"));
    if !path.exists() {
        return Err(ThemeFileError::NotFound {
            name: name.into(),
            dir: dir.to_path_buf(),
        });
    }
    ThemeFile::load_from(&path)
}

/// List available theme names (file stems) in a directory. Missing
/// directory → empty vec. Non-TOML files are ignored.
pub fn list_themes(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                return None;
            }
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn parses_minimal_theme_with_meta_and_colors_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(
            &path,
            r##"
[meta]
name = "Tiny"
variant = "dark"

[colors]
primary = "#7AA2F7"
"##,
        )
        .unwrap();
        let t = ThemeFile::load_from(&path).unwrap();
        assert_eq!(t.meta.name, "Tiny");
        assert_eq!(t.meta.variant, "dark");
        assert!(t.meta.light_pair.is_none());
        assert_eq!(t.colors.primary.as_deref(), Some("#7AA2F7"));
        assert!(t.colors.bg.is_none(), "unspecified → None");
        assert!(t.bar.is_none());
    }

    #[test]
    fn parses_full_theme_with_all_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.toml");
        std::fs::write(
            &path,
            r##"
[meta]
name = "Full"
author = "tester"
variant = "dark"
light_pair = "full-light"

[colors]
bg = "#1A1B26"
bg_dark = "#16161E"
surface = "#24283B"
surface_raised = "#2F3549"
primary = "#7AA2F7"
on_primary = "#1A1B26"

[health]
stale_pill = "#737AA2"
error_pill = "#B8806A"

[bar]
opacity = 0.85
blur_radius = 24
height_full = 48
height_compact = 28

[typography]
font_text = "Iosevka"
font_mono = "JetBrains Mono"

[icons]
style = "duotone"
duotone_secondary = "#565F89"
"##,
        )
        .unwrap();
        let t = ThemeFile::load_from(&path).unwrap();
        assert_eq!(t.meta.author.as_deref(), Some("tester"));
        assert_eq!(t.meta.light_pair.as_deref(), Some("full-light"));
        assert_eq!(t.colors.bg_dark.as_deref(), Some("#16161E"));
        assert_eq!(t.colors.surface_raised.as_deref(), Some("#2F3549"));
        assert_eq!(t.colors.on_primary.as_deref(), Some("#1A1B26"));
        let bar = t.bar.unwrap();
        assert_eq!(bar.opacity, Some(0.85));
        assert_eq!(bar.height_full, Some(48));
        assert_eq!(t.health.unwrap().error_pill.as_deref(), Some("#B8806A"));
        assert_eq!(
            t.typography.unwrap().font_text.as_deref(),
            Some("Iosevka")
        );
        assert_eq!(t.icons.unwrap().style.as_deref(), Some("duotone"));
    }

    #[test]
    fn bad_variant_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.toml");
        std::fs::write(
            &path,
            r##"
[meta]
name = "Weird"
variant = "twilight"

[colors]
"##,
        )
        .unwrap();
        let err = ThemeFile::load_from(&path).unwrap_err();
        assert!(matches!(err, ThemeFileError::BadVariant { .. }));
    }

    #[test]
    fn missing_meta_section_is_a_toml_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.toml");
        std::fs::write(&path, "[colors]\nprimary = \"#000\"\n").unwrap();
        let err = ThemeFile::load_from(&path).unwrap_err();
        assert!(matches!(err, ThemeFileError::Toml { .. }));
    }

    #[test]
    fn load_theme_by_name_resolves_filename() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "cool.toml",
            r##"[meta]
name = "Cool"
variant = "dark"
[colors]
primary = "#123456"
"##,
        );
        let t = load_theme(dir.path(), "cool").unwrap();
        assert_eq!(t.meta.name, "Cool");
        assert_eq!(t.colors.primary.as_deref(), Some("#123456"));
    }

    #[test]
    fn load_theme_unknown_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_theme(dir.path(), "missing").unwrap_err();
        assert!(matches!(err, ThemeFileError::NotFound { .. }));
    }

    #[test]
    fn list_themes_returns_sorted_stems() {
        let dir = tempfile::tempdir().unwrap();
        let stub = r##"[meta]
name = "x"
variant = "dark"
[colors]
"##;
        write(dir.path(), "warm-dark.toml", stub);
        write(dir.path(), "neutral-dark.toml", stub);
        write(dir.path(), "not-a-theme.txt", "ignored");
        let names = list_themes(dir.path());
        assert_eq!(names, vec!["neutral-dark".to_string(), "warm-dark".to_string()]);
    }

    #[test]
    fn list_themes_missing_dir_returns_empty() {
        let names = list_themes(Path::new("/nope/nope/themes"));
        assert!(names.is_empty());
    }

    #[test]
    fn bootstrap_writes_bundled_themes_into_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("themes");
        let report = bootstrap_themes(&target, false).unwrap();

        assert_eq!(report.dir, target);
        assert_eq!(report.written.len(), BUILTIN_THEMES.len());
        assert!(report.skipped.is_empty());

        // Every bundled theme name lands as a parseable TOML file.
        for (name, _) in BUILTIN_THEMES {
            let path = target.join(format!("{name}.toml"));
            assert!(path.exists(), "missing {name}.toml after bootstrap");
            let parsed = ThemeFile::load_from(&path).unwrap();
            assert!(!parsed.meta.name.is_empty());
        }
    }

    #[test]
    fn bootstrap_skips_existing_files_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("themes");
        std::fs::create_dir_all(&target).unwrap();
        let warm_dark = target.join("warm-dark.toml");
        std::fs::write(&warm_dark, "user-edited").unwrap();

        let report = bootstrap_themes(&target, false).unwrap();
        assert!(report.skipped.contains(&"warm-dark".to_string()));
        assert!(!report.written.contains(&"warm-dark".to_string()));
        assert_eq!(std::fs::read_to_string(&warm_dark).unwrap(), "user-edited");
    }

    #[test]
    fn bootstrap_force_overwrites_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("themes");
        std::fs::create_dir_all(&target).unwrap();
        let warm_dark = target.join("warm-dark.toml");
        std::fs::write(&warm_dark, "user-edited").unwrap();

        let report = bootstrap_themes(&target, true).unwrap();
        assert!(report.written.contains(&"warm-dark".to_string()));
        assert!(report.skipped.is_empty());
        let body = std::fs::read_to_string(&warm_dark).unwrap();
        assert_ne!(body, "user-edited", "force should overwrite");
        assert!(ThemeFile::load_from(&warm_dark).is_ok());
    }
}
