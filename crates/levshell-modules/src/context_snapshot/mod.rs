//! Context save/restore (spec §2.12.2).
//!
//! v1 scope (confirmed with user 2026-04-17):
//!
//! - Capture the sway window tree: per-window `workspace / app_id /
//!   title / cmdline / floating`.
//! - Restore: best-effort, silent. Match existing windows by `app_id`
//!   (prefer same title), move them to saved workspaces. Unmatched
//!   windows with a stored cmdline are re-launched via sway's `exec`.
//! - Surface = `levshell-ctl context {save,restore,list,delete} <name>`.
//!   Keybind / palette integration comes later.
//!
//! The module is **function-based**, not a [`Module`] trait impl —
//! there's nothing for it to tick on, publish, or subscribe to. The
//! daemon's ctl dispatcher calls into these async functions directly.
//!
//! [`Module`]: levshell_core::Module

pub mod capture;
pub mod model;
pub mod restore;

use std::path::{Path, PathBuf};

use swayipc_async::Connection;
use thiserror::Error;

pub use model::{ContextSnapshot, WindowSnapshot};
pub use restore::{LaunchAction, MoveAction, RestorePlan};

/// Errors from the high-level save/restore API. Individual failures
/// (one window won't move, one launch exec'd but crashed) are absorbed
/// into the summary string — these variants cover hard failures that
/// prevent the operation from completing at all.
#[derive(Debug, Error)]
pub enum ContextSnapshotError {
    #[error("sway ipc error: {0}")]
    Sway(#[from] swayipc_async::Error),
    #[error("snapshot {name:?} not found in {dir}")]
    NotFound { name: String, dir: PathBuf },
    #[error("invalid snapshot name {name:?}: must be non-empty and contain only a-z0-9-_")]
    InvalidName { name: String },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error on {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialize snapshot: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Summary of a completed save / restore / delete operation — fed back
/// to the ctl client so the user sees "saved 12 windows" / "moved 3,
/// launched 2, skipped 1" instead of a silent OK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSummary {
    pub message: String,
}

impl OperationSummary {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Default contexts directory: `$XDG_STATE_HOME/levshell/contexts` or
/// `~/.local/state/levshell/contexts`. Mirrors the warmup state layout.
pub fn default_contexts_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("levshell/contexts")
}

/// Validate a user-supplied snapshot name. We constrain it to the
/// character class `[a-zA-Z0-9_-]+` so it's safe as a filename on any
/// filesystem and can't traverse directories.
fn validate_name(name: &str) -> Result<(), ContextSnapshotError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ContextSnapshotError::InvalidName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

fn path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// List saved snapshot names (filename stems). Missing directory is
/// treated as "no snapshots".
pub fn list_snapshots(dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "context-snapshot: read_dir failed");
            }
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    names
}

/// Delete a named snapshot. Returns `NotFound` when the file doesn't
/// exist. Deliberately does *not* warn; removing a missing snapshot is
/// a user-visible error, not a silent no-op.
pub fn delete_snapshot(name: &str, dir: &Path) -> Result<OperationSummary, ContextSnapshotError> {
    validate_name(name)?;
    let path = path_for(dir, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(OperationSummary::new(format!("deleted {name}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ContextSnapshotError::NotFound {
                name: name.to_owned(),
                dir: dir.to_path_buf(),
            })
        }
        Err(e) => Err(ContextSnapshotError::Io { path, source: e }),
    }
}

fn write_snapshot(snap: &ContextSnapshot, dir: &Path) -> Result<(), ContextSnapshotError> {
    std::fs::create_dir_all(dir).map_err(|e| ContextSnapshotError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    let path = path_for(dir, &snap.name);
    let body = serde_json::to_string_pretty(snap).map_err(ContextSnapshotError::Serialize)?;
    std::fs::write(&path, body).map_err(|e| ContextSnapshotError::Io { path, source: e })
}

fn read_snapshot(name: &str, dir: &Path) -> Result<ContextSnapshot, ContextSnapshotError> {
    let path = path_for(dir, name);
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ContextSnapshotError::NotFound {
                name: name.to_owned(),
                dir: dir.to_path_buf(),
            });
        }
        Err(e) => return Err(ContextSnapshotError::Io { path, source: e }),
    };
    serde_json::from_str(&body).map_err(|e| ContextSnapshotError::Parse { path, source: e })
}

/// Capture the current sway tree into a named snapshot on disk.
///
/// Opens a fresh sway-ipc connection (cheap) — the daemon's
/// SwayWorkspaceModule has its own long-lived connection for events
/// and we don't want to entangle them.
pub async fn save_current(
    name: &str,
    dir: &Path,
) -> Result<OperationSummary, ContextSnapshotError> {
    validate_name(name)?;
    let mut conn = Connection::new().await?;
    let tree = conn.get_tree().await?;
    let snapshot = capture::capture_from_tree(name, &tree, capture::read_cmdline_from_proc);
    let window_count = snapshot.windows.len();
    let workspace_count = snapshot
        .windows
        .iter()
        .map(|w| w.workspace.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    write_snapshot(&snapshot, dir)?;
    Ok(OperationSummary::new(format!(
        "saved {name}: {window_count} window(s) across {workspace_count} workspace(s)"
    )))
}

/// Apply a saved snapshot. Best-effort: existing windows with matching
/// app_id + title are moved to the saved workspace; windows not
/// currently running get re-launched via `sway exec` if we captured
/// their cmdline. Windows with no match and no cmdline are skipped —
/// the summary reports the count.
pub async fn restore_snapshot(
    name: &str,
    dir: &Path,
) -> Result<OperationSummary, ContextSnapshotError> {
    validate_name(name)?;
    let snapshot = read_snapshot(name, dir)?;
    let mut conn = Connection::new().await?;
    let tree = conn.get_tree().await?;
    let live = restore::flatten_live_windows(&tree);
    let plan = restore::plan_restore(&snapshot, &live);

    let mut move_ok = 0u32;
    let mut move_err = 0u32;
    for m in &plan.moves {
        let cmd = format!(
            "[con_id={}] move to workspace {}",
            m.con_id,
            sway_quote(&m.target_workspace)
        );
        match conn.run_command(&cmd).await {
            Ok(results) if results.iter().all(|r| r.is_ok()) => {
                move_ok = move_ok.saturating_add(1);
            }
            Ok(results) => {
                move_err = move_err.saturating_add(1);
                for r in results.iter().filter_map(|r| r.as_ref().err()) {
                    tracing::warn!(cmd = %cmd, error = %r, "context-snapshot: move failed");
                }
            }
            Err(e) => {
                move_err = move_err.saturating_add(1);
                tracing::warn!(cmd = %cmd, error = %e, "context-snapshot: move ipc error");
            }
        }
    }

    let mut launch_ok = 0u32;
    let mut launch_err = 0u32;
    for l in &plan.launches {
        // Focus the target workspace first so sway places the new
        // window there. Then exec the saved cmdline.
        let focus = format!("workspace {}", sway_quote(&l.target_workspace));
        let exec = format!(
            "exec {}",
            shell_quote_cmdline(&l.cmdline)
        );
        let combined = format!("{focus}; {exec}");
        match conn.run_command(&combined).await {
            Ok(results) if results.iter().all(|r| r.is_ok()) => {
                launch_ok = launch_ok.saturating_add(1);
            }
            Ok(results) => {
                launch_err = launch_err.saturating_add(1);
                for r in results.iter().filter_map(|r| r.as_ref().err()) {
                    tracing::warn!(cmd = %combined, error = %r, "context-snapshot: launch failed");
                }
            }
            Err(e) => {
                launch_err = launch_err.saturating_add(1);
                tracing::warn!(cmd = %combined, error = %e, "context-snapshot: launch ipc error");
            }
        }
    }

    // Finally, focus the saved workspace so the user lands where they
    // were at capture time. Best-effort — if it fails we don't report
    // that as a hard error.
    if let Some(ws) = plan.focused_workspace.as_deref() {
        let cmd = format!("workspace {}", sway_quote(ws));
        if let Err(e) = conn.run_command(&cmd).await {
            tracing::warn!(cmd = %cmd, error = %e, "context-snapshot: final focus failed");
        }
    }

    let mut bits = Vec::new();
    bits.push(format!("moved {move_ok}"));
    bits.push(format!("launched {launch_ok}"));
    if plan.skipped_unrestorable > 0 {
        bits.push(format!("skipped {}", plan.skipped_unrestorable));
    }
    if move_err > 0 {
        bits.push(format!("move errors {move_err}"));
    }
    if launch_err > 0 {
        bits.push(format!("launch errors {launch_err}"));
    }
    Ok(OperationSummary::new(format!(
        "restored {name}: {}",
        bits.join(", ")
    )))
}

/// Quote a workspace name for sway's command parser. Sway workspaces
/// can contain spaces (e.g. `3:code`), so we wrap in double quotes
/// and escape embedded `"` and `\`.
fn sway_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Shell-quote a full argv list into a single line suitable for `exec`.
/// Sway passes the string to `/bin/sh -c`, so we need POSIX-safe
/// single-quote escaping.
fn shell_quote_cmdline(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote_one(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote_one(s: &str) -> String {
    // Single-quote wraps everything literally except for embedded single
    // quotes, which we close + escape + reopen: foo'bar → 'foo'\''bar'.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn snap(name: &str) -> ContextSnapshot {
        ContextSnapshot {
            name: name.into(),
            captured_at: Utc::now(),
            focused_workspace: None,
            windows: vec![],
        }
    }

    #[test]
    fn validate_name_accepts_reasonable_names() {
        assert!(validate_name("research").is_ok());
        assert!(validate_name("paper_1").is_ok());
        assert!(validate_name("lit-review").is_ok());
        assert!(validate_name("A123-b").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty_and_special_chars() {
        assert!(validate_name("").is_err());
        assert!(validate_name("foo bar").is_err());
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("foo/bar").is_err());
        assert!(validate_name("x.y").is_err());
    }

    #[test]
    fn list_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(&snap("a"), dir.path()).unwrap();
        write_snapshot(&snap("b"), dir.path()).unwrap();
        assert_eq!(list_snapshots(dir.path()), vec!["a", "b"]);

        let s = delete_snapshot("a", dir.path()).unwrap();
        assert!(s.message.contains("deleted"));
        assert_eq!(list_snapshots(dir.path()), vec!["b"]);

        // Double-delete surfaces NotFound.
        assert!(matches!(
            delete_snapshot("a", dir.path()),
            Err(ContextSnapshotError::NotFound { .. })
        ));
    }

    #[test]
    fn list_snapshots_on_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-there");
        assert!(list_snapshots(&missing).is_empty());
    }

    #[test]
    fn write_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = snap("x");
        s.focused_workspace = Some("3:code".into());
        s.windows.push(WindowSnapshot {
            workspace: "3:code".into(),
            app_id: "neovide".into(),
            title: "draft.md".into(),
            cmdline: Some(vec!["neovide".into()]),
            floating: false,
        });
        write_snapshot(&s, dir.path()).unwrap();
        let back = read_snapshot("x", dir.path()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn read_snapshot_surfaces_parse_error_for_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.json"), "not json").unwrap();
        let res = read_snapshot("bad", dir.path());
        assert!(matches!(res, Err(ContextSnapshotError::Parse { .. })));
    }

    #[test]
    fn sway_quote_escapes_backslash_and_quote() {
        assert_eq!(sway_quote("foo"), r#""foo""#);
        assert_eq!(sway_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(sway_quote(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote_one("foo"), "'foo'");
        assert_eq!(shell_quote_one("it's"), "'it'\\''s'");
        assert_eq!(
            shell_quote_cmdline(&["neovide".into(), "draft.md".into()]),
            "'neovide' 'draft.md'"
        );
    }
}
