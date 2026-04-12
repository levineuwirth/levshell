//! [`DataStore`] — owns the SQLite connection and serves the async query API.
//!
//! All public methods on `DataStore` (defined here and across `crate::ops::*`)
//! follow the same shape: clone the inner `Arc<Mutex<Connection>>`, hand it to
//! [`tokio::task::spawn_blocking`], lock the mutex inside the closure, run the
//! synchronous `rusqlite` calls, and propagate the result back to the caller.
//! `std::sync::Mutex` is correct here because the lock is only held inside
//! `spawn_blocking`, never across an `.await` point.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};

use crate::error::Result;

const MIGRATION_001: &str = include_str!("../migrations/001_initial.sql");

#[derive(Clone)]
pub struct DataStore {
    pub(crate) inner: Arc<Mutex<Connection>>,
}

impl DataStore {
    /// Open the database at `path`, set per-connection pragmas, and run
    /// embedded migrations to bring the schema to the latest version.
    /// Creates parent directories if they do not exist.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || Self::open_blocking(&path)).await?
    }

    /// Open an in-memory database. Used for tests; not exposed in the
    /// daemon's normal startup path.
    pub async fn open_in_memory() -> Result<Self> {
        tokio::task::spawn_blocking(|| {
            let mut conn = Connection::open_in_memory()?;
            Self::configure_connection(&conn)?;
            migrations().to_latest(&mut conn)?;
            Ok(Self {
                inner: Arc::new(Mutex::new(conn)),
            })
        })
        .await?
    }

    fn open_blocking(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut conn = Connection::open(path)?;
        Self::configure_connection(&conn)?;
        migrations().to_latest(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    fn configure_connection(conn: &Connection) -> Result<()> {
        // journal_mode is a one-time pragma that returns the new mode in a
        // result row, so it has to go through query_row rather than execute.
        let mode: String =
            conn.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(actual = %mode, "failed to enable WAL journal mode");
        }
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// Helper used by every operation: clones the connection handle into a
    /// blocking task, locks the mutex, and runs the closure with a `&mut
    /// Connection`. The mutex is poisoned only if a previous closure panicked,
    /// which is treated as a programming error and surfaced as a panic here as
    /// well — recovering from a poisoned database mutex is unsafe.
    pub(crate) async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().expect("data store mutex poisoned");
            f(&mut guard)
        })
        .await?
    }
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(MIGRATION_001)])
}

// Manual Debug to keep the connection out of debug output.
impl std::fmt::Debug for DataStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataStore").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_runs_against_in_memory_db() {
        // Round-trip: building Migrations and applying them on a throwaway
        // in-memory connection catches typos in the embedded .sql file
        // without needing tokio.
        let mut conn = Connection::open_in_memory().unwrap();
        DataStore::configure_connection(&conn).unwrap();
        migrations().to_latest(&mut conn).unwrap();
    }
}
