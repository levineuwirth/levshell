//! [`SyncAdapter`] trait and supporting types (§3.3.4 of the spec).
//!
//! Every external-tool integration (Obsidian, Zotero, AnkiConnect, CalDAV)
//! implements this trait. Adapters are isolated from the rest of the daemon:
//! they read from the external source, write to [`levshell_data::DataStore`],
//! and return a [`SyncReport`] describing what changed. The
//! [`crate::SyncEngine`] is responsible for scheduling, timeouts, and bus
//! eventing.
//!
//! # Design notes
//!
//! The spec (§3.3.4) lists `pull(since) / push(deltas) / full_import()` as
//! separate trait methods returning `Vec<SyncDelta>`. In practice each
//! adapter's logic naturally flows as a single "read external → diff against
//! store → upsert" pass, with the engine not needing to mediate. We therefore
//! fold pull + apply into a single `sync()` method that takes a
//! [`SyncContext`] (holding the data store and the last-sync cursor) and
//! returns a [`SyncReport`]. `push()` stays separate because its payload is
//! different (locally-changed entities pushed back out, for bidirectional
//! adapters); it defaults to a no-op for import-only adapters.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use levshell_data::{DataStore, EntityType};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SyncError>;

/// All failure modes the sync framework exposes. Variants are deliberately
/// broad — adapters wrap their own error types in `External` rather than
/// leaking third-party error types through the trait boundary.
#[derive(Debug, Error)]
pub enum SyncError {
    /// The external tool is not installed, not reachable, or not configured.
    /// The engine surfaces this via [`SyncStatus::Unavailable`] and does not
    /// treat it as a hard failure.
    #[error("sync source unavailable: {0}")]
    Unavailable(String),

    /// An error from the external tool's API or format (HTTP 500, malformed
    /// file, missing required field, etc.).
    #[error("external tool error: {0}")]
    External(String),

    /// An error from [`levshell_data`] while writing the synced payload.
    #[error("data store error: {0}")]
    Data(#[from] levshell_data::DataError),

    /// A filesystem error — typically from a filesystem-backed adapter
    /// (Obsidian vault, local ICS file).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The adapter exceeded its timeout budget. The engine catches this and
    /// marks the adapter [`SyncStatus::Stale`].
    #[error("sync timeout")]
    Timeout,

    /// JSON encode/decode while translating an entity payload.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Health state the engine reports on behalf of an adapter. Mirrors the
/// widget health states defined in spec §5.2.1 so a sync failure can
/// directly map to a widget's degraded-state treatment.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SyncStatus {
    /// Last sync completed within the configured interval without errors.
    Healthy,
    /// Last sync is older than expected, but the adapter is still reachable.
    Stale,
    /// The backing service is not installed or not configured. The engine
    /// skips scheduling instead of looping on failed probes.
    Unavailable,
    /// Last sync attempt returned an error. The engine keeps the loop alive
    /// so subsequent successes can recover to [`Self::Healthy`].
    Error,
}

/// Per-tick context handed to [`SyncAdapter::sync`]. Each call gets a
/// fresh context, so changes to `since` between ticks are picked up without
/// mutating the adapter.
#[derive(Debug, Clone)]
pub struct SyncContext {
    /// The unified data store. Adapters write entities, tags, and sync
    /// metadata here directly. The store is cheap to clone (internally an
    /// `Arc<Mutex<Connection>>`).
    pub store: DataStore,

    /// Cursor: the watermark to fetch changes from. `None` means this is
    /// the first sync of this process; the adapter is free to fall back to
    /// its own persistent watermark (e.g. filesystem mtime, or the
    /// `MAX(sync_metadata.last_synced_at)` for its provider).
    pub since: Option<DateTime<Utc>>,
}

/// Single conflict detected during an apply step. Emitted as a
/// [`levshell_core::Event::SyncConflict`] by the engine.
#[derive(Debug, Clone)]
pub struct SyncConflict {
    pub entity_type: EntityType,
    pub external_id: String,
    pub reason: String,
}

/// Summary of a single sync pass. Feeds the engine's
/// [`levshell_core::Event::SyncCompleted`] publication.
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub upserted: u32,
    pub deleted: u32,
    pub conflicts: Vec<SyncConflict>,
}

impl SyncReport {
    pub fn is_empty(&self) -> bool {
        self.upserted == 0 && self.deleted == 0 && self.conflicts.is_empty()
    }
}

/// The trait every external-tool integration implements. Object-safe via
/// `async-trait` so the engine can hold `Arc<dyn SyncAdapter>` values.
#[async_trait]
pub trait SyncAdapter: Send + Sync + 'static {
    /// Provider name used in logs, bus events, and as the
    /// `sync_metadata.provider` key. Stable across versions — renaming
    /// strands previously-synced entities because their sync_metadata row
    /// points at the old name.
    fn name(&self) -> &str;

    /// Entity types this adapter populates. Used by the engine for
    /// diagnostics and by the conflict resolver to decide which tables to
    /// inspect.
    fn entity_types(&self) -> Vec<EntityType>;

    /// How often the engine should call [`Self::sync`]. Defaults to 5
    /// minutes; battery mode applies a global multiplier (spec §5.3).
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    /// How long a single sync pass is allowed to take. Exceeding this
    /// produces a [`SyncError::Timeout`]. Defaults to 30s.
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Cheap health check — verify the external tool is reachable and
    /// configured. Called before each [`Self::sync`] so a transient outage
    /// does not roll the adapter into [`SyncStatus::Error`] permanently.
    async fn probe(&self, ctx: &SyncContext) -> SyncStatus;

    /// The main sync operation. Contract:
    /// 1. Fetch changes from the external tool (respect `ctx.since` if
    ///    set — full import on `None`).
    /// 2. For each change, insert or update the corresponding entity in
    ///    `ctx.store` and upsert the matching `sync_metadata` row.
    /// 3. Return a [`SyncReport`] counting upserts, deletes, and conflicts.
    ///
    /// Conflict handling: v1 always applies last-write-wins. Detected
    /// conflicts are returned in the report (not silently suppressed) so
    /// the engine can surface them as bus events.
    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport>;

    /// Push locally-changed entities back to the external tool. Default
    /// no-op covers import-only adapters. Bidirectional adapters override.
    async fn push(&self, _ctx: &SyncContext) -> Result<()> {
        Ok(())
    }
}
