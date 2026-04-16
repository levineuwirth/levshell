//! Zotero sync adapter (§5.2, §6.2 item 2).
//!
//! Maps a user's Zotero library (a local `zotero.sqlite` database) into
//! the unified data model's `refs` table. Each non-trashed, non-attachment,
//! non-note Zotero item becomes one [`Reference`]. Child PDF attachments
//! surface as the parent's `pdf_path`; Zotero tags become `entity_tags`.
//!
//! This adapter is **import-only** — writing back to Zotero requires the
//! Zotero API, which is out of scope for v1 (spec §5.2 names it as
//! optional future work).
//!
//! ## Sync algorithm
//!
//! 1. Open `database_path` with `SQLITE_OPEN_READ_ONLY`. Zotero uses
//!    WAL journaling so a running Zotero can coexist with our readers.
//! 2. Run five bulk queries to build a flat [`read::RawItem`] list.
//! 3. For each item, translate to a [`NewReference`] / [`ReferencePatch`]
//!    and check `sync_metadata` by `(provider="zotero", external_id=item key)`.
//!    - **Absent**: insert a new Reference + sync_metadata row.
//!    - **Present, sync_hash == item.dateModified**: skip.
//!    - **Present, sync_hash differs**: update; emit conflict event if
//!      the local reference was edited since last sync (last-write-wins).
//! 4. After the walk, any sync_metadata row whose external_id isn't in
//!    the current Zotero item set points at a trashed or hard-deleted
//!    item. Drop the reference + its metadata.
//!
//! ## Citekey strategy
//!
//! The `refs.citekey` column is `UNIQUE`. Zotero itself doesn't store a
//! user-visible citekey — users who want stable BibTeX-style keys run
//! Better BibTeX, which writes `Citation Key: xyz` into each item's
//! `extra` field. We parse that header when present; otherwise we fall
//! back to Zotero's 8-char uppercase item key, which is guaranteed
//! unique. On collision (two items parsed out to the same BBT key —
//! rare but possible mid-edit), we fall back to the item key for the
//! loser and log a warning.

pub mod config;
pub mod read;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_config::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
use levshell_data::{
    EntityType, NewReference, ReferencePatch, SyncDirection, SyncMetadata,
};
use thiserror::Error;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub use config::{ZoteroConfig, ZoteroConfigError};

/// Provider name. Persisted in `sync_metadata.provider` — never change
/// without a migration.
pub const PROVIDER_NAME: &str = "zotero";

/// Errors that escape the rusqlite / read-layer boundary. All get
/// funneled into [`SyncError::External`] at the adapter boundary.
#[derive(Debug, Error)]
pub(crate) enum ZoteroError {
    #[error("opening zotero database {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("reading zotero database: {0}")]
    Query(#[from] rusqlite::Error),
}

impl From<ZoteroError> for SyncError {
    fn from(e: ZoteroError) -> Self {
        SyncError::External(e.to_string())
    }
}

/// The adapter. Config lives behind a lock so hot-reload can swap it
/// between ticks without tearing down the adapter task — same pattern
/// as [`crate::obsidian::ObsidianAdapter`].
pub struct ZoteroAdapter {
    config: Arc<RwLock<ZoteroConfig>>,
}

impl ZoteroAdapter {
    pub fn new(config: ZoteroConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
        }
    }

    pub fn reload_config(&self, new: ZoteroConfig) {
        let mut guard = self.config.write().expect("zotero config lock poisoned");
        *guard = new;
    }

    pub fn current_config(&self) -> ZoteroConfig {
        self.snapshot()
    }

    fn snapshot(&self) -> ZoteroConfig {
        self.config.read().expect("zotero config lock poisoned").clone()
    }
}

#[async_trait]
impl SyncAdapter for ZoteroAdapter {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Reference]
    }

    fn poll_interval(&self) -> Duration {
        self.snapshot().poll_interval()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        let config = self.snapshot();
        if !config.enabled {
            return SyncStatus::Unavailable;
        }
        if !config.database_path.is_file() {
            return SyncStatus::Unavailable;
        }
        SyncStatus::Healthy
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let config = self.snapshot();
        if !config.enabled {
            return Ok(SyncReport::default());
        }

        let items = read_items_blocking(&config.database_path).await?;

        // Drop items belonging to filtered-out libraries (if a filter
        // is configured). We keep the filter after the SQL read so the
        // sync_metadata deletion pass still sees the full Zotero set —
        // otherwise toggling `libraries` would leave stale rows behind.
        let kept: Vec<read::RawItem> = items
            .into_iter()
            .filter(|it| config.matches_library(it.library_id))
            .collect();

        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external_id: HashMap<String, SyncMetadata> =
            existing.into_iter().map(|m| (m.external_id.clone(), m)).collect();

        // Track citekeys we've already claimed this sync pass so a
        // second item with the same BBT key can fall back to the item
        // key instead of failing the UNIQUE constraint.
        let mut claimed_citekeys: HashSet<String> = HashSet::new();

        let mut report = SyncReport::default();

        for item in &kept {
            let payload = build_payload(item, &mut claimed_citekeys);

            let meta = by_external_id.remove(&item.key);
            match meta {
                None => apply_insert(ctx, &payload, &mut report).await?,
                Some(prev) => {
                    if prev.sync_hash.as_deref() == Some(item.date_modified.as_str()) {
                        continue;
                    }
                    apply_update(ctx, &payload, &prev, &mut report).await?;
                }
            }
        }

        // Anything left in `by_external_id` is an item we previously
        // synced but Zotero no longer reports — trashed, hard-deleted,
        // or moved to a now-filtered library.
        for (_, stale) in by_external_id {
            apply_delete(ctx, &stale, &mut report).await?;
        }

        Ok(report)
    }
}

/// Payload handed between the translate step and the apply step.
struct Payload<'a> {
    item: &'a read::RawItem,
    citekey: String,
    title: String,
    authors: Vec<String>,
    year: Option<i32>,
    venue: Option<String>,
    doi: Option<String>,
    abstract_text: Option<String>,
    pdf_path: Option<String>,
    tags: Vec<String>,
}

fn build_payload<'a>(
    item: &'a read::RawItem,
    claimed_citekeys: &mut HashSet<String>,
) -> Payload<'a> {
    let bbt_key = item
        .extra
        .as_deref()
        .and_then(read::citekey_from_extra);
    let citekey = match bbt_key {
        Some(k) if !claimed_citekeys.contains(&k) => k,
        Some(colliding) => {
            tracing::warn!(
                zotero_key = %item.key,
                colliding_key = %colliding,
                "bbt citekey already claimed this sync; falling back to zotero item key"
            );
            item.key.clone()
        }
        None => item.key.clone(),
    };
    claimed_citekeys.insert(citekey.clone());

    let title = item
        .title
        .clone()
        .unwrap_or_else(|| format!("[untitled {}]", item.key));
    let authors: Vec<String> = item.creators.iter().map(|c| c.display()).collect();
    let year = item.date.as_deref().and_then(read::year_from_date);

    Payload {
        item,
        citekey,
        title,
        authors,
        year,
        venue: item.publication_title.clone(),
        doi: item.doi.clone(),
        abstract_text: item.abstract_note.clone(),
        pdf_path: item.pdf_path.clone(),
        tags: item.tags.clone(),
    }
}

async fn apply_insert(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    report: &mut SyncReport,
) -> Result<()> {
    let new = NewReference {
        title: payload.title.clone(),
        authors: payload.authors.clone(),
        year: payload.year,
        venue: payload.venue.clone(),
        doi: payload.doi.clone(),
        citekey: payload.citekey.clone(),
        abstract_text: payload.abstract_text.clone(),
        pdf_path: payload.pdf_path.clone(),
        reading_progress: None,
        annotations: Vec::new(),
        project_id: None,
    };
    let reference = match ctx.store.insert_reference(new).await {
        Ok(r) => r,
        Err(e) => {
            // A UNIQUE constraint on citekey can fire if a native
            // Levshell reference already claimed the same key. Log
            // loudly and skip — dropping the whole sync on one bad
            // item would be worse.
            tracing::warn!(
                provider = PROVIDER_NAME,
                zotero_key = %payload.item.key,
                citekey = %payload.citekey,
                error = %e,
                "insert_reference failed; skipping this item"
            );
            return Ok(());
        }
    };

    write_tags(ctx, reference.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: reference.id,
            entity_type: EntityType::Reference,
            provider: PROVIDER_NAME.into(),
            external_id: payload.item.key.clone(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.item.date_modified.clone()),
        })
        .await?;

    report.upserted += 1;
    Ok(())
}

async fn apply_update(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    prev: &SyncMetadata,
    report: &mut SyncReport,
) -> Result<()> {
    let Some(reference) = ctx.store.get_reference(prev.entity_id).await? else {
        // Metadata is orphaned (entity deleted manually). Recreate
        // rather than chase the ghost.
        return apply_insert(ctx, payload, report).await;
    };

    if reference.updated_at > prev.last_synced_at {
        report.conflicts.push(SyncConflict {
            entity_type: EntityType::Reference,
            external_id: payload.item.key.clone(),
            reason: "local reference modified since last sync; external copy wins (v1)".into(),
        });
    }

    ctx.store
        .update_reference(
            reference.id,
            ReferencePatch {
                title: Some(payload.title.clone()),
                authors: Some(payload.authors.clone()),
                year: Some(payload.year),
                venue: Some(payload.venue.clone()),
                doi: Some(payload.doi.clone()),
                citekey: Some(payload.citekey.clone()),
                abstract_text: Some(payload.abstract_text.clone()),
                pdf_path: Some(payload.pdf_path.clone()),
                annotations: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await?;

    replace_tags(ctx, reference.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: reference.id,
            entity_type: EntityType::Reference,
            provider: PROVIDER_NAME.into(),
            external_id: payload.item.key.clone(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.item.date_modified.clone()),
        })
        .await?;

    report.upserted += 1;
    Ok(())
}

async fn apply_delete(
    ctx: &SyncContext,
    stale: &SyncMetadata,
    report: &mut SyncReport,
) -> Result<()> {
    ctx.store.delete_reference(stale.entity_id).await?;
    ctx.store
        .clear_sync_metadata(stale.entity_id, EntityType::Reference, PROVIDER_NAME)
        .await?;
    report.deleted += 1;
    Ok(())
}

async fn write_tags(ctx: &SyncContext, ref_id: Uuid, tags: &[String]) -> Result<()> {
    for tag in tags {
        ctx.store
            .add_tag(ref_id, EntityType::Reference, tag)
            .await?;
    }
    Ok(())
}

async fn replace_tags(ctx: &SyncContext, ref_id: Uuid, new_tags: &[String]) -> Result<()> {
    let current = ctx.store.get_tags(ref_id, EntityType::Reference).await?;
    for old in &current {
        if !new_tags.iter().any(|n| n == old) {
            ctx.store
                .remove_tag(ref_id, EntityType::Reference, old.as_str())
                .await?;
        }
    }
    for new in new_tags {
        if !current.iter().any(|c| c == new) {
            ctx.store
                .add_tag(ref_id, EntityType::Reference, new.as_str())
                .await?;
        }
    }
    Ok(())
}

/// Run the rusqlite read in a blocking thread so the async adapter
/// doesn't tie up a tokio worker. `rusqlite::Connection` isn't `Send`
/// across `.await`, so we build it, use it, and drop it all inside
/// `spawn_blocking`.
async fn read_items_blocking(path: &Path) -> Result<Vec<read::RawItem>> {
    let owned = path.to_path_buf();
    let items = tokio::task::spawn_blocking(move || -> std::result::Result<_, ZoteroError> {
        let conn = read::open_readonly(&owned)?;
        read::read_items(&conn)
    })
    .await
    .map_err(|e| SyncError::External(format!("zotero blocking task joined with error: {e}")))??;
    Ok(items)
}

/// Hot-reload supervisor for a [`ZoteroAdapter`]. Mirrors
/// [`crate::obsidian::ObsidianConfigWatcher`]: watches the sync
/// directory for `zotero.toml` and applies changes atomically.
pub struct ZoteroConfigWatcher {
    _watcher: ConfigWatcher,
    task: JoinHandle<()>,
}

impl ZoteroConfigWatcher {
    pub fn spawn(
        adapter: Arc<ZoteroAdapter>,
        sync_dir: &Path,
    ) -> std::result::Result<Self, WatcherError> {
        let (watcher, mut rx) = watch_config_dir(sync_dir)?;
        let config_path = sync_dir.join("zotero.toml");
        let task = tokio::spawn(async move {
            while let Some(change) = rx.recv().await {
                match change {
                    ConfigChange::Upserted(path) if path == config_path => {
                        match ZoteroConfig::load_from(&path) {
                            Ok(new) => {
                                tracing::info!(
                                    database = %new.database_path.display(),
                                    enabled = new.enabled,
                                    poll_secs = new.poll_interval_secs,
                                    "zotero hot-reload: applying new config"
                                );
                                adapter.reload_config(new);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "zotero hot-reload: parse failed; adapter unchanged"
                                );
                            }
                        }
                    }
                    ConfigChange::Removed(path) if path == config_path => {
                        tracing::info!(
                            "zotero hot-reload: config file removed; disabling adapter"
                        );
                        let mut current = adapter.current_config();
                        current.enabled = false;
                        adapter.reload_config(current);
                    }
                    _ => {}
                }
            }
            tracing::debug!("zotero hot-reload: watcher channel closed");
        });
        Ok(Self {
            _watcher: watcher,
            task,
        })
    }

    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

impl std::fmt::Debug for ZoteroConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoteroConfigWatcher").finish_non_exhaustive()
    }
}
