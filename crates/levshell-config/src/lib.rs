//! Levshell configuration loader.
//!
//! Loads layered TOML configuration from `~/.config/levshell/`. Phase 1.2
//! ships only the **profile loader**: every `*.toml` file under
//! `profiles/` is parsed into a [`levshell_context::Profile`]. Later phases
//! will extend this crate to cover module settings, rules, themes, and
//! inotify-based hot reload.
//!
//! The loader is intentionally forgiving: a malformed profile file logs a
//! warning and is skipped rather than failing the whole load. The daemon
//! starts with whatever profiles parsed successfully.

#![forbid(unsafe_code)]

pub mod profiles;
pub mod settings;
pub mod themes;
pub mod watcher;

pub use profiles::{
    default_config_base, default_profiles_dir, default_sync_dir, load_profile_file,
    load_profiles_from_dir, spawn_profile_watcher, ConfigError, ProfileFile, ProfileWatcher,
};
pub use themes::{
    bootstrap_themes, default_themes_dir, list_themes, load_theme, BarTokens, BootstrapReport,
    ColorTokens, HealthTokens, ThemeFile, ThemeFileError, ThemeMeta,
    TypographyTokens, BUILTIN_THEMES,
};
pub use settings::{default_settings_path, load_settings, write_setting, Settings};
pub use watcher::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
