//! Obsidian vault sync adapter.
//!
//! Maps a user's Obsidian vault (a directory of Markdown files) into the
//! unified data model's `notes` table. Each `.md` file becomes one Note,
//! with YAML frontmatter tags written to `entity_tags` and the relative
//! path stored as the sync_metadata `external_id`.
//!
//! This adapter is **import-only** for v1: edits inside Levshell's native
//! knowledge base (a Phase 3 replacement) are the bidirectional path.
//!
//! ## Sync algorithm
//!
//! Each `sync()` call does a full vault walk:
//!
//! 1. Enumerate every `*.md` file under the vault, skipping excluded dirs.
//! 2. For each file: parse frontmatter, compute content hash, check
//!    `sync_metadata` by `(provider, external_id)`.
//!    - **Absent**: insert a new Note and create a sync_metadata row.
//!    - **Present and hash unchanged**: skip (most files, most of the time).
//!    - **Present and hash changed**: conflict check against the local
//!      Note's `updated_at`; apply last-write-wins; emit a conflict entry
//!      in the report when the local copy had been modified since last
//!      sync.
//! 3. After the walk, compare the full vault file set against
//!    `list_sync_metadata_by_provider("obsidian")` — every stale
//!    sync_metadata row points at a vault file that no longer exists.
//!    Delete both the Note and the metadata.
//!
//! ## Why not incremental via `ctx.since`?
//!
//! A full vault walk is O(vault size) per sync. For vaults up to ~10k
//! notes this completes in well under the adapter's default 30s timeout.
//! A true incremental adapter would need a filesystem watcher
//! (inotify/fsevents) and shared state between ticks — worth doing in
//! a later phase, not v1.

mod config;
mod frontmatter;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_data::{EntityType, NewNote, NotePatch, SyncDirection, SyncMetadata};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub use config::ObsidianConfig;

/// Provider name. Persisted in sync_metadata.provider rows — never change
/// without a migration.
pub const PROVIDER_NAME: &str = "obsidian";

/// Filesystem walker result: one vault file we want to sync.
#[derive(Debug, Clone)]
struct VaultEntry {
    /// Path relative to the vault root, with forward slashes. Stored as
    /// the sync_metadata external_id; stable across platforms.
    external_id: String,
    /// Absolute path for reading.
    absolute: PathBuf,
}

/// Parsed + hashed payload for a single vault file, derived once and
/// threaded through the insert / update branches.
struct Payload<'a> {
    entry: &'a VaultEntry,
    title: String,
    body: String,
    hash: String,
    tags: Vec<String>,
}

/// The adapter itself. Holds immutable config; all mutable state lives
/// in the data store.
pub struct ObsidianAdapter {
    config: ObsidianConfig,
}

impl ObsidianAdapter {
    pub fn new(config: ObsidianConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ObsidianConfig {
        &self.config
    }

    fn walk_vault(&self) -> io::Result<Vec<VaultEntry>> {
        let mut out = Vec::new();
        if !self.config.vault_path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("vault path not a directory: {:?}", self.config.vault_path),
            ));
        }
        walk_dir(&self.config.vault_path, &self.config.vault_path, &self.config, &mut out)?;
        Ok(out)
    }
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    config: &ObsidianConfig,
    out: &mut Vec<VaultEntry>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        if file_type.is_symlink() {
            // Skip symlinks; following them risks cycles and accidentally
            // escaping the vault. Users with intentional symlink layouts
            // can revisit this later.
            continue;
        }
        if file_type.is_dir() {
            if config.is_excluded_dir(&name) {
                continue;
            }
            walk_dir(root, &path, config, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Normalize to forward slashes so the external_id is
        // platform-independent. `Path::display()` can interleave
        // platform separators, so explicitly reconstruct from components.
        let external_id = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(VaultEntry {
            external_id,
            absolute: path,
        });
    }
    Ok(())
}

fn content_hash(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn title_from_path(external_id: &str) -> String {
    external_id
        .rsplit('/')
        .next()
        .and_then(|s| s.strip_suffix(".md"))
        .unwrap_or(external_id)
        .to_string()
}

#[async_trait]
impl SyncAdapter for ObsidianAdapter {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Note]
    }

    fn poll_interval(&self) -> Duration {
        self.config.poll_interval()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        if !self.config.enabled {
            return SyncStatus::Unavailable;
        }
        if !self.config.vault_path.is_dir() {
            return SyncStatus::Unavailable;
        }
        SyncStatus::Healthy
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        if !self.config.enabled {
            return Ok(SyncReport::default());
        }

        let entries = self.walk_vault().map_err(SyncError::from)?;

        // Index existing sync_metadata rows by external_id so the main
        // loop is O(1) per file instead of one DB query per file.
        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external_id: HashMap<String, SyncMetadata> =
            existing.into_iter().map(|m| (m.external_id.clone(), m)).collect();

        let mut report = SyncReport::default();

        for entry in &entries {
            let raw = match fs::read_to_string(&entry.absolute) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(
                        provider = PROVIDER_NAME,
                        path = %entry.absolute.display(),
                        error = %err,
                        "failed to read vault file; skipping"
                    );
                    continue;
                }
            };
            let (fm, body) = frontmatter::parse(&raw);
            let hash = content_hash(body);
            let title = fm
                .title
                .clone()
                .unwrap_or_else(|| title_from_path(&entry.external_id));
            let payload = Payload {
                entry,
                title,
                body: body.to_string(),
                hash,
                tags: fm.tags,
            };

            let meta = by_external_id.remove(&entry.external_id);
            match meta {
                None => {
                    apply_insert(ctx, &payload, &mut report).await?;
                }
                Some(prev) => {
                    if prev.sync_hash.as_deref() == Some(payload.hash.as_str()) {
                        // Unchanged; nothing to do.
                        continue;
                    }
                    apply_update(ctx, &payload, &prev, &mut report).await?;
                }
            }
        }

        // Anything left in `by_external_id` no longer exists in the vault.
        for (_, stale) in by_external_id {
            apply_delete(ctx, &stale, &mut report).await?;
        }

        Ok(report)
    }
}

async fn apply_insert(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    report: &mut SyncReport,
) -> Result<()> {
    let note = ctx
        .store
        .insert_note(NewNote {
            title: payload.title.clone(),
            content: payload.body.clone(),
            project_id: None,
        })
        .await?;

    write_tags(ctx, note.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: note.id,
            entity_type: EntityType::Note,
            provider: PROVIDER_NAME.into(),
            external_id: payload.entry.external_id.clone(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.hash.clone()),
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
    let note = ctx.store.get_note(prev.entity_id).await?;

    let Some(note) = note else {
        // The sync_metadata row is orphaned — its entity disappeared via
        // `ON DELETE SET NULL` or a raw DELETE. Recreate instead of
        // chasing the ghost.
        apply_insert(ctx, payload, report).await?;
        return Ok(());
    };

    // Conflict detection: if the local note was modified AFTER the last
    // sync, both sides have diverged. V1 applies last-write-wins
    // (external overrides) and surfaces the conflict as an event.
    if note.updated_at > prev.last_synced_at {
        report.conflicts.push(SyncConflict {
            entity_type: EntityType::Note,
            external_id: payload.entry.external_id.clone(),
            reason: "local note modified since last sync; external copy wins (v1)".into(),
        });
    }

    ctx.store
        .update_note(
            note.id,
            NotePatch {
                title: Some(payload.title.clone()),
                content: Some(payload.body.clone()),
                ..Default::default()
            },
        )
        .await?;

    replace_tags(ctx, note.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: note.id,
            entity_type: EntityType::Note,
            provider: PROVIDER_NAME.into(),
            external_id: payload.entry.external_id.clone(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.hash.clone()),
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
    ctx.store.delete_note(stale.entity_id).await?;
    ctx.store
        .clear_sync_metadata(stale.entity_id, EntityType::Note, PROVIDER_NAME)
        .await?;
    report.deleted += 1;
    Ok(())
}

async fn write_tags(ctx: &SyncContext, note_id: Uuid, tags: &[String]) -> Result<()> {
    for tag in tags {
        ctx.store.add_tag(note_id, EntityType::Note, tag).await?;
    }
    Ok(())
}

async fn replace_tags(ctx: &SyncContext, note_id: Uuid, new_tags: &[String]) -> Result<()> {
    // Remove tags no longer in the frontmatter, then add any new ones.
    // Small enough set per note that a diff-then-apply is cheaper than
    // clearing and re-adding unconditionally.
    let current = ctx.store.get_tags(note_id, EntityType::Note).await?;
    for old in &current {
        if !new_tags.iter().any(|n| n == old) {
            ctx.store
                .remove_tag(note_id, EntityType::Note, old.as_str())
                .await?;
        }
    }
    for new in new_tags {
        if !current.iter().any(|c| c == new) {
            ctx.store
                .add_tag(note_id, EntityType::Note, new.as_str())
                .await?;
        }
    }
    Ok(())
}
