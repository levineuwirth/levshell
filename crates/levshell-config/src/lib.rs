//! Levshell configuration loader.
//!
//! Loads layered TOML configuration from `~/.config/levshell/` (global theme,
//! per-module settings, sync adapter configs, project declarations, profiles,
//! rules, and themes). Watches the directory tree via inotify so config edits
//! take effect without a daemon restart.

#![forbid(unsafe_code)]
