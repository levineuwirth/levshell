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
mod links;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_config::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
use levshell_data::{EntityType, NewNote, NotePatch, SyncDirection, SyncMetadata};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub use config::{ObsidianConfig, ObsidianConfigError};

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

/// The adapter. Config lives behind a read-write lock so it can be
/// hot-reloaded without tearing down the adapter task. Every trait
/// method takes a snapshot clone at its entry point; in-flight calls
/// continue with the config they observed at start, while subsequent
/// calls see any reloaded values.
pub struct ObsidianAdapter {
    config: Arc<RwLock<ObsidianConfig>>,
}

impl ObsidianAdapter {
    pub fn new(config: ObsidianConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Replace the adapter's config atomically. See
    /// [`crate::obsidian`] module docs for hot-reload semantics. The
    /// caller is expected to hold a strong `Arc<ObsidianAdapter>` (the
    /// sync engine does) so the adapter isn't torn down while the
    /// daemon's watcher calls this.
    pub fn reload_config(&self, new: ObsidianConfig) {
        let mut guard = self.config.write().expect("obsidian config lock poisoned");
        *guard = new;
    }

    /// Snapshot clone of the current live config. Useful for tests,
    /// diagnostics, and watchers that want to inspect the active
    /// settings. The value is a clone — mutations on the returned
    /// struct do NOT affect the adapter.
    pub fn current_config(&self) -> ObsidianConfig {
        self.snapshot()
    }

    /// Snapshot clone of the current config. Used internally so each
    /// method grabs a consistent view for its entire execution.
    fn snapshot(&self) -> ObsidianConfig {
        self.config.read().expect("obsidian config lock poisoned").clone()
    }

    fn walk_vault(config: &ObsidianConfig) -> io::Result<Vec<VaultEntry>> {
        let mut out = Vec::new();
        if !config.vault_path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("vault path not a directory: {:?}", config.vault_path),
            ));
        }
        walk_dir(&config.vault_path, &config.vault_path, config, &mut out)?;
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
        self.snapshot().poll_interval()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        let config = self.snapshot();
        if !config.enabled {
            return SyncStatus::Unavailable;
        }
        if !config.vault_path.is_dir() {
            return SyncStatus::Unavailable;
        }
        SyncStatus::Healthy
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let config = self.snapshot();
        if !config.enabled {
            return Ok(SyncReport::default());
        }

        let entries = Self::walk_vault(&config).map_err(SyncError::from)?;

        // Index existing sync_metadata rows by external_id so the main
        // loop is O(1) per file instead of one DB query per file.
        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external_id: HashMap<String, SyncMetadata> =
            existing.into_iter().map(|m| (m.external_id.clone(), m)).collect();

        let mut report = SyncReport::default();
        // Notes whose outgoing wiki-link edges need to be (re)populated
        // after the main upsert pass — either new notes or existing
        // notes whose content changed.
        let mut link_updates: Vec<(Uuid, Vec<String>)> = Vec::new();

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
            let links = links::extract(body);
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
                    if let Some(id) = apply_insert(ctx, &payload, &mut report).await? {
                        link_updates.push((id, links));
                    }
                }
                Some(prev) => {
                    if prev.sync_hash.as_deref() == Some(payload.hash.as_str()) {
                        // Unchanged body → unchanged outgoing links.
                        continue;
                    }
                    if let Some(id) = apply_update(ctx, &payload, &prev, &mut report).await? {
                        link_updates.push((id, links));
                    }
                }
            }
        }

        // Anything left in `by_external_id` no longer exists in the vault.
        // Also strip its outgoing + incoming wiki-link edges so the graph
        // never references ghost notes.
        for (_, stale) in by_external_id {
            apply_delete(ctx, &stale, &mut report).await?;
        }

        // Build a resolution index over the CURRENT vault's
        // sync_metadata — the main loop may have inserted new rows
        // since the snapshot above, so re-fetch. Index by exact
        // external_id for path-prefixed links and by basename
        // (filename stem) for bare links.
        if !link_updates.is_empty() {
            populate_wiki_link_edges(ctx, &link_updates).await?;
        }

        Ok(report)
    }
}

/// For every (source_note, raw_link_targets) pair, clear the source's
/// existing outgoing `wiki_link` edges and add a fresh edge per
/// resolved target. Targets that don't match any note are silently
/// skipped (dangling references are a normal state in a vault — the
/// user may be planning to create the target next).
async fn populate_wiki_link_edges(
    ctx: &SyncContext,
    updates: &[(Uuid, Vec<String>)],
) -> Result<()> {
    let metas = ctx
        .store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await?;

    // Exact external_id lookup, e.g. [[research/paper]] → "research/paper.md".
    let mut by_path: HashMap<String, Uuid> = HashMap::new();
    // Basename lookup (filename without extension), e.g. [[paper]] → the
    // first sync_metadata whose external_id ends with "paper.md". First
    // insertion wins so the iteration order of sync_metadata determines
    // collisions — acceptable for v1; users with name collisions can
    // disambiguate with folder prefixes.
    let mut by_basename: HashMap<String, Uuid> = HashMap::new();
    for m in &metas {
        by_path.insert(m.external_id.clone(), m.entity_id);
        let basename = m
            .external_id
            .rsplit('/')
            .next()
            .unwrap_or(&m.external_id)
            .trim_end_matches(".md");
        by_basename.entry(basename.to_string()).or_insert(m.entity_id);
    }

    for (source_id, targets) in updates {
        ctx.store
            .clear_relations_from(*source_id, EntityType::Note, "wiki_link")
            .await?;
        for raw_target in targets {
            let resolved = resolve_link_target(raw_target, &by_path, &by_basename);
            if let Some(target_id) = resolved {
                if target_id == *source_id {
                    // Self-link: skip. Obsidian allows it but it
                    // produces a degenerate self-loop in the graph.
                    continue;
                }
                ctx.store
                    .add_relation(
                        *source_id,
                        EntityType::Note,
                        target_id,
                        EntityType::Note,
                        "wiki_link",
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

fn resolve_link_target(
    raw: &str,
    by_path: &HashMap<String, Uuid>,
    by_basename: &HashMap<String, Uuid>,
) -> Option<Uuid> {
    // Try exact path match first, with and without the .md suffix
    // (Obsidian users write `[[folder/note]]`, we stored
    // `folder/note.md`).
    if let Some(id) = by_path.get(raw) {
        return Some(*id);
    }
    let with_ext = format!("{raw}.md");
    if let Some(id) = by_path.get(&with_ext) {
        return Some(*id);
    }
    // Fall back to basename lookup. Strip any trailing `.md` so both
    // `[[foo]]` and `[[foo.md]]` resolve identically.
    let basename = raw.trim_end_matches(".md");
    by_basename.get(basename).copied()
}

async fn apply_insert(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    report: &mut SyncReport,
) -> Result<Option<Uuid>> {
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
    Ok(Some(note.id))
}

async fn apply_update(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    prev: &SyncMetadata,
    report: &mut SyncReport,
) -> Result<Option<Uuid>> {
    let note = ctx.store.get_note(prev.entity_id).await?;

    let Some(note) = note else {
        // The sync_metadata row is orphaned — its entity disappeared via
        // `ON DELETE SET NULL` or a raw DELETE. Recreate instead of
        // chasing the ghost.
        return apply_insert(ctx, payload, report).await;
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
    Ok(Some(note.id))
}

async fn apply_delete(
    ctx: &SyncContext,
    stale: &SyncMetadata,
    report: &mut SyncReport,
) -> Result<()> {
    // Clear wiki-link edges in both directions before the row goes
    // away so the graph never references a ghost note. Incoming edges
    // (other notes that link to this one) will silently drop their
    // dangling reference; the corresponding source notes re-emit
    // their own outgoing edges on their next content change.
    ctx.store
        .clear_relations_from(stale.entity_id, EntityType::Note, "wiki_link")
        .await?;
    let incoming = ctx
        .store
        .list_relations_to(stale.entity_id, EntityType::Note)
        .await?;
    for edge in incoming {
        ctx.store
            .remove_relation(
                edge.source_id,
                edge.source_type,
                edge.target_id,
                edge.target_type,
                edge.kind,
            )
            .await?;
    }

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

/// Hot-reload supervisor for an [`ObsidianAdapter`]. Watches the
/// configured sync directory and calls
/// [`ObsidianAdapter::reload_config`] whenever `obsidian.toml` is
/// created / modified. Spec §3.9 requires all configuration to be
/// hot-reloadable via inotify.
///
/// Hold this for as long as the adapter is in use; dropping it stops
/// the watch but leaves the adapter's last-applied config in place.
pub struct ObsidianConfigWatcher {
    _watcher: ConfigWatcher,
    task: JoinHandle<()>,
}

impl ObsidianConfigWatcher {
    /// Watch `sync_dir` (typically `$XDG_CONFIG_HOME/levshell/sync/`)
    /// for changes to `obsidian.toml` and apply them to `adapter`.
    /// Other files in the directory (e.g. future `zotero.toml`) are
    /// ignored by this watcher.
    ///
    /// When the config file is deleted the adapter is flipped to
    /// `enabled = false` rather than having its vault_path etc.
    /// cleared — that keeps the adapter task alive (probes return
    /// `Unavailable` cleanly) so a later re-add of the file can
    /// recover without a daemon restart.
    pub fn spawn(
        adapter: Arc<ObsidianAdapter>,
        sync_dir: &Path,
    ) -> std::result::Result<Self, WatcherError> {
        let (watcher, mut rx) = watch_config_dir(sync_dir)?;
        let config_path = sync_dir.join("obsidian.toml");
        let task = tokio::spawn(async move {
            while let Some(change) = rx.recv().await {
                match change {
                    ConfigChange::Upserted(path) if path == config_path => {
                        match ObsidianConfig::load_from(&path) {
                            Ok(new) => {
                                tracing::info!(
                                    vault = %new.vault_path.display(),
                                    enabled = new.enabled,
                                    poll_secs = new.poll_interval_secs,
                                    "obsidian hot-reload: applying new config"
                                );
                                adapter.reload_config(new);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "obsidian hot-reload: parse failed; adapter unchanged"
                                );
                            }
                        }
                    }
                    ConfigChange::Removed(path) if path == config_path => {
                        tracing::info!(
                            "obsidian hot-reload: config file removed; disabling adapter"
                        );
                        let mut current = adapter.current_config();
                        current.enabled = false;
                        adapter.reload_config(current);
                    }
                    _ => {}
                }
            }
            tracing::debug!("obsidian hot-reload: watcher channel closed");
        });
        Ok(Self {
            _watcher: watcher,
            task,
        })
    }

    /// Abort the background task and await its exit. Call on clean
    /// shutdown; `drop` alone stops the task without awaiting.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

impl std::fmt::Debug for ObsidianConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObsidianConfigWatcher").finish_non_exhaustive()
    }
}
