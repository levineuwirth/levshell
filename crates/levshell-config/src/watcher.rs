//! Directory watcher for hot-reloadable `*.toml` configuration files.
//!
//! Wraps [`notify::RecommendedWatcher`] behind an async-friendly
//! interface: the caller receives a [`ConfigWatcher`] (which must be
//! held to keep the underlying OS watch alive) and a
//! [`mpsc::UnboundedReceiver`] that yields [`ConfigChange`] events
//! whenever a `*.toml` file in the watched directory is created,
//! modified, or removed. Non-TOML files are silently ignored.
//!
//! # Ordering & duplicates
//!
//! This module intentionally does **not** debounce or deduplicate.
//! Editors often rewrite a file as `{remove, create}` or produce
//! rapid-fire `modify` events; consumers must treat `Upserted` as
//! idempotent (re-parse + re-upsert) and `Removed` as advisory (log and
//! refuse to destroy data from filesystem absence alone — the user may
//! simply be renaming). This policy keeps the watcher itself stateless
//! and dependency-free on per-consumer behaviour.
//!
//! # Lifecycle
//!
//! Dropping the [`ConfigWatcher`] stops the watch. The OS watch is
//! tied to the lifetime of the `RecommendedWatcher` that the struct
//! holds; no explicit shutdown is required.

use std::path::{Path, PathBuf};

use notify::event::EventKind as NotifyEventKind;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;

/// Notification sent when a `*.toml` file under the watched directory
/// changes. The consumer decides how to respond — typically
/// `load_*_file(path)` + upsert for `Upserted`, or a warning log for
/// `Removed` (data-preserving policy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChange {
    /// The file was created or modified. Path is absolute.
    Upserted(PathBuf),
    /// The file was deleted or renamed away. Path is absolute.
    Removed(PathBuf),
}

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("notify error watching {path}: {source}")]
    Notify {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

/// Active watch over a directory. Keep this alive — dropping it stops
/// the OS watch. The paired [`mpsc::UnboundedReceiver`] returned by
/// [`watch_config_dir`] delivers events from the background notify
/// thread.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigWatcher").finish_non_exhaustive()
    }
}

/// Begin watching `dir` for `*.toml` create/modify/delete events and
/// return a handle plus the receive end of an unbounded channel.
///
/// `dir` is watched **non-recursively** — only files immediately under
/// `dir` produce events. Subdirectories are ignored. This matches how
/// Levshell's config layout uses flat per-resource directories
/// (`profiles/`, `projects/`, `sync/`).
pub fn watch_config_dir(
    dir: &Path,
) -> Result<(ConfigWatcher, mpsc::UnboundedReceiver<ConfigChange>), WatcherError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let dir_owned = dir.to_path_buf();
    let dispatch_dir = dir_owned.clone();
    let handler = move |res: Result<Event, notify::Error>| {
        let event = match res {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    dir = %dispatch_dir.display(),
                    error = %err,
                    "config watcher: backend error"
                );
                return;
            }
        };
        dispatch(&tx, event);
    };
    let mut watcher = RecommendedWatcher::new(handler, Config::default()).map_err(|e| {
        WatcherError::Notify {
            path: dir_owned.clone(),
            source: e,
        }
    })?;
    watcher
        .watch(&dir_owned, RecursiveMode::NonRecursive)
        .map_err(|e| WatcherError::Notify {
            path: dir_owned,
            source: e,
        })?;
    Ok((
        ConfigWatcher {
            _watcher: watcher,
        },
        rx,
    ))
}

fn dispatch(tx: &mpsc::UnboundedSender<ConfigChange>, event: Event) {
    let toml_paths: Vec<PathBuf> = event
        .paths
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    if toml_paths.is_empty() {
        return;
    }
    match event.kind {
        NotifyEventKind::Create(_) | NotifyEventKind::Modify(_) => {
            for path in toml_paths {
                if tx.send(ConfigChange::Upserted(path)).is_err() {
                    return;
                }
            }
        }
        NotifyEventKind::Remove(_) => {
            for path in toml_paths {
                if tx.send(ConfigChange::Removed(path)).is_err() {
                    return;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Collect events for a bounded window so the test doesn't hang on
    /// absent events. Returns whatever arrived within the timeout.
    async fn drain(
        rx: &mut mpsc::UnboundedReceiver<ConfigChange>,
        window: Duration,
    ) -> Vec<ConfigChange> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(change)) => out.push(change),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        out
    }

    #[tokio::test(flavor = "current_thread")]
    async fn creating_a_toml_emits_upserted() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut rx) = watch_config_dir(dir.path()).unwrap();

        let path = dir.path().join("a.toml");
        std::fs::write(&path, "name = \"A\"\n").unwrap();

        let events = drain(&mut rx, Duration::from_secs(3)).await;
        assert!(
            events.iter().any(|e| matches!(e, ConfigChange::Upserted(p) if p == &path)),
            "expected Upserted for {}, got {events:?}",
            path.display()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn modifying_a_toml_emits_upserted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.toml");
        std::fs::write(&path, "name = \"v1\"\n").unwrap();

        let (_watcher, mut rx) = watch_config_dir(dir.path()).unwrap();

        // Small settle so the initial-content write's events don't leak
        // into our drain window on slower filesystems.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = drain(&mut rx, Duration::from_millis(50)).await;

        std::fs::write(&path, "name = \"v2\"\n").unwrap();
        let events = drain(&mut rx, Duration::from_secs(3)).await;
        assert!(
            events.iter().any(|e| matches!(e, ConfigChange::Upserted(p) if p == &path)),
            "expected Upserted for modify, got {events:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn removing_a_toml_emits_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.toml");
        std::fs::write(&path, "name = \"R\"\n").unwrap();

        let (_watcher, mut rx) = watch_config_dir(dir.path()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = drain(&mut rx, Duration::from_millis(50)).await;

        std::fs::remove_file(&path).unwrap();
        let events = drain(&mut rx, Duration::from_secs(3)).await;
        assert!(
            events.iter().any(|e| matches!(e, ConfigChange::Removed(p) if p == &path)),
            "expected Removed for delete, got {events:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_toml_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut rx) = watch_config_dir(dir.path()).unwrap();

        std::fs::write(dir.path().join("README.md"), "# docs").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let events = drain(&mut rx, Duration::from_millis(500)).await;
        assert!(
            events.is_empty(),
            "expected no events for non-TOML files, got {events:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn watcher_errors_surface_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let err = watch_config_dir(&missing).unwrap_err();
        assert!(matches!(err, WatcherError::Notify { .. }));
    }
}
