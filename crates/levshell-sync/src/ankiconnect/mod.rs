//! AnkiConnect sync adapter (§5.2, §6.2 item 2).
//!
//! Third of the four v1 sync adapters. Imports cards from the
//! community AnkiConnect plugin into the unified data model's
//! `flashcards` table. AnkiConnect listens on `localhost:8765` by
//! default and speaks a plain JSON-RPC protocol over HTTP — we lean
//! on it rather than scraping Anki's SQLite directly so users don't
//! need to close Anki for sync to work.
//!
//! ## Sync algorithm
//!
//! 1. Probe via `version` action. Failure → `Unavailable`.
//! 2. `findCards` with the configured query (default
//!    `"-is:suspended"`) → list of card IDs.
//! 3. Batched `cardsInfo` + `notesInfo` → rich card records with tags.
//! 4. For each card:
//!    - translate to `NewFlashcard`/`FlashcardPatch`
//!    - look up `(provider="ankiconnect", external_id=cardId)` in
//!      sync_metadata
//!    - absent: insert and write tags
//!    - present with same `sync_hash`: skip
//!    - present with different `sync_hash`: update + replace tags
//! 5. Delete flashcards whose sync_metadata isn't in the current
//!    card set — same pattern as Obsidian/Zotero.
//!
//! ## Due-date heuristic
//!
//! AnkiConnect's `due` field is queue-dependent and not directly
//! convertible to an absolute timestamp without the collection's
//! creation date (AnkiConnect doesn't currently expose that in a
//! standard way). v1 uses a deliberately-approximate but stable
//! heuristic documented on [`due_at_from_card`] — it's good enough
//! for "review due counter" widgets and cross-tool queries, but not
//! for precise scheduling. A native SRS replacement (spec Phase 3)
//! will get exact scheduling.

pub mod client;
pub mod config;
pub mod types;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use levshell_config::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
use levshell_data::{
    EntityType, FlashcardPatch, NewFlashcard, SyncDirection, SyncMetadata,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub use client::{AnkiClient, AnkiClientError, AnkiConnectHttpClient, ANKICONNECT_API_VERSION};
pub use config::{AnkiConnectConfig, AnkiConnectConfigError};
pub use types::{CardInfo, FieldValue, NoteInfo};

/// Provider name. Persisted in `sync_metadata.provider` — never change
/// without a migration.
pub const PROVIDER_NAME: &str = "ankiconnect";

/// Maximum cards per `cardsInfo` batch. AnkiConnect handles larger
/// arrays but payload size scales linearly; chunking keeps per-request
/// timeouts honest.
const BATCH_SIZE: usize = 500;

impl From<AnkiClientError> for SyncError {
    fn from(e: AnkiClientError) -> Self {
        match e {
            AnkiClientError::Http { .. } => SyncError::Unavailable(e.to_string()),
            _ => SyncError::External(e.to_string()),
        }
    }
}

/// The adapter. Config behind an `RwLock` so hot-reload swaps it
/// atomically (same pattern as Obsidian / Zotero). The HTTP client is
/// rebuilt whenever the config changes because the endpoint /
/// timeout / api_key are baked into it.
pub struct AnkiConnectAdapter {
    config: Arc<RwLock<AnkiConnectConfig>>,
    /// Currently-live client. `Arc<dyn AnkiClient>` so tests can plug
    /// in a mock via [`Self::with_client`].
    client: Arc<RwLock<Arc<dyn AnkiClient>>>,
}

impl AnkiConnectAdapter {
    /// Build an adapter with a production HTTP client. Returns an
    /// error only when reqwest fails to build its Client (extremely
    /// rare — misconfigured TLS roots, etc.); the adapter handles
    /// per-request failures at probe/sync time.
    pub fn new(config: AnkiConnectConfig) -> std::result::Result<Self, AnkiClientError> {
        let client = AnkiConnectHttpClient::new(
            config.endpoint.clone(),
            config.request_timeout(),
            config.api_key.clone(),
        )?;
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            client: Arc::new(RwLock::new(Arc::new(client))),
        })
    }

    /// Build with an arbitrary [`AnkiClient`] — used by unit tests to
    /// inject a [`MockAnkiClient`].
    pub fn with_client(config: AnkiConnectConfig, client: Arc<dyn AnkiClient>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            client: Arc::new(RwLock::new(client)),
        }
    }

    /// Swap in a new config. Rebuilds the HTTP client so endpoint /
    /// timeout changes actually take effect.
    pub fn reload_config(&self, new: AnkiConnectConfig) {
        let http = match AnkiConnectHttpClient::new(
            new.endpoint.clone(),
            new.request_timeout(),
            new.api_key.clone(),
        ) {
            Ok(c) => Arc::new(c) as Arc<dyn AnkiClient>,
            Err(e) => {
                tracing::warn!(
                    endpoint = %new.endpoint,
                    error = %e,
                    "ankiconnect hot-reload: rebuilding http client failed; keeping previous client"
                );
                return;
            }
        };
        {
            let mut guard = self.config.write().expect("ankiconnect config lock poisoned");
            *guard = new;
        }
        let mut guard = self.client.write().expect("ankiconnect client lock poisoned");
        *guard = http;
    }

    pub fn current_config(&self) -> AnkiConnectConfig {
        self.config.read().expect("ankiconnect config lock poisoned").clone()
    }

    fn snapshot_client(&self) -> Arc<dyn AnkiClient> {
        self.client.read().expect("ankiconnect client lock poisoned").clone()
    }
}

#[async_trait]
impl SyncAdapter for AnkiConnectAdapter {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Flashcard]
    }

    fn poll_interval(&self) -> Duration {
        self.current_config().poll_interval()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        if !self.current_config().enabled {
            return SyncStatus::Unavailable;
        }
        let client = self.snapshot_client();
        match client.version().await {
            Ok(v) if v >= ANKICONNECT_API_VERSION => SyncStatus::Healthy,
            Ok(_) => SyncStatus::Unavailable,
            Err(_) => SyncStatus::Unavailable,
        }
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let config = self.current_config();
        if !config.enabled {
            return Ok(SyncReport::default());
        }
        let client = self.snapshot_client();

        let card_ids = client.find_cards(&config.deck_filter).await?;
        let cards = fetch_in_batches(&*client, &card_ids).await?;

        // Pull note-level metadata (tags) for the unique note set —
        // one card's note is often shared across its siblings, so we
        // dedupe before the `notesInfo` call.
        let note_ids: Vec<i64> = {
            let mut set = std::collections::BTreeSet::new();
            for c in &cards {
                set.insert(c.note_id);
            }
            set.into_iter().collect()
        };
        let notes = client.notes_info(&note_ids).await?;
        let tags_by_note: HashMap<i64, Vec<String>> =
            notes.into_iter().map(|n| (n.note_id, n.tags)).collect();

        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external_id: HashMap<String, SyncMetadata> = existing
            .into_iter()
            .map(|m| (m.external_id.clone(), m))
            .collect();

        let mut report = SyncReport::default();
        let now = Utc::now();

        for card in &cards {
            if !card.is_syncable() {
                continue;
            }
            let external_id = card.card_id.to_string();
            let tags = tags_by_note.get(&card.note_id).cloned().unwrap_or_default();
            let payload = Payload {
                card,
                tags,
                due_at: due_at_from_card(card, now),
                sync_hash: card.modified.to_string(),
            };

            let meta = by_external_id.remove(&external_id);
            match meta {
                None => apply_insert(ctx, &payload, &mut report).await?,
                Some(prev) => {
                    if prev.sync_hash.as_deref() == Some(payload.sync_hash.as_str()) {
                        continue;
                    }
                    apply_update(ctx, &payload, &prev, &mut report).await?;
                }
            }
        }

        // Anything left in by_external_id no longer matches the
        // current findCards filter — either suspended, deleted, or
        // moved out of the configured deck. Drop it.
        for (_, stale) in by_external_id {
            apply_delete(ctx, &stale, &mut report).await?;
        }

        Ok(report)
    }
}

async fn fetch_in_batches(
    client: &dyn AnkiClient,
    ids: &[i64],
) -> Result<Vec<CardInfo>> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_SIZE) {
        let mut batch = client.cards_info(chunk).await?;
        out.append(&mut batch);
    }
    Ok(out)
}

struct Payload<'a> {
    card: &'a CardInfo,
    tags: Vec<String>,
    due_at: DateTime<Utc>,
    sync_hash: String,
}

async fn apply_insert(
    ctx: &SyncContext,
    payload: &Payload<'_>,
    report: &mut SyncReport,
) -> Result<()> {
    let (front, back) = (
        strip_html(&payload.card.question),
        strip_html(&payload.card.answer),
    );
    let new = NewFlashcard {
        front,
        back,
        linked_note_id: None,
        linked_ref_id: None,
        project_id: None,
        interval_days: payload.card.interval.max(0) as f64,
        ease_factor: (payload.card.factor as f64 / 1000.0).max(1.3),
        due_at: payload.due_at,
    };
    let card = ctx.store.insert_flashcard(new).await?;
    write_tags(ctx, card.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: card.id,
            entity_type: EntityType::Flashcard,
            provider: PROVIDER_NAME.into(),
            external_id: payload.card.card_id.to_string(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.sync_hash.clone()),
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
    let Some(flashcard) = ctx.store.get_flashcard(prev.entity_id).await? else {
        return apply_insert(ctx, payload, report).await;
    };

    if flashcard.updated_at > prev.last_synced_at {
        report.conflicts.push(SyncConflict {
            entity_type: EntityType::Flashcard,
            external_id: payload.card.card_id.to_string(),
            reason: "local flashcard modified since last sync; external copy wins (v1)".into(),
        });
    }

    let patch = FlashcardPatch {
        front: Some(strip_html(&payload.card.question)),
        back: Some(strip_html(&payload.card.answer)),
        interval_days: Some(payload.card.interval.max(0) as f64),
        ease_factor: Some((payload.card.factor as f64 / 1000.0).max(1.3)),
        due_at: Some(payload.due_at),
        review_count: Some(payload.card.reps),
        ..Default::default()
    };
    ctx.store.update_flashcard(flashcard.id, patch).await?;
    replace_tags(ctx, flashcard.id, &payload.tags).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: flashcard.id,
            entity_type: EntityType::Flashcard,
            provider: PROVIDER_NAME.into(),
            external_id: payload.card.card_id.to_string(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(payload.sync_hash.clone()),
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
    ctx.store.delete_flashcard(stale.entity_id).await?;
    ctx.store
        .clear_sync_metadata(stale.entity_id, EntityType::Flashcard, PROVIDER_NAME)
        .await?;
    report.deleted += 1;
    Ok(())
}

async fn write_tags(ctx: &SyncContext, id: Uuid, tags: &[String]) -> Result<()> {
    for t in tags {
        ctx.store.add_tag(id, EntityType::Flashcard, t).await?;
    }
    Ok(())
}

async fn replace_tags(ctx: &SyncContext, id: Uuid, new_tags: &[String]) -> Result<()> {
    let current = ctx.store.get_tags(id, EntityType::Flashcard).await?;
    for old in &current {
        if !new_tags.iter().any(|n| n == old) {
            ctx.store
                .remove_tag(id, EntityType::Flashcard, old.as_str())
                .await?;
        }
    }
    for new in new_tags {
        if !current.iter().any(|c| c == new) {
            ctx.store
                .add_tag(id, EntityType::Flashcard, new.as_str())
                .await?;
        }
    }
    Ok(())
}

/// Translate AnkiConnect's queue-dependent `due` / `interval` fields
/// into an absolute `DateTime<Utc>`. See the module docs for the
/// precision caveat.
///
/// - `queue == 1` (learning): `due` is a unix timestamp (seconds).
///   Convert directly; if parse fails, fall back to now.
/// - `queue == 2 || queue == 3` (review / day-learning): `due` is
///   "days since collection creation" — we don't have the collection
///   epoch, so we approximate next-review as `now + interval days`.
///   Stable (the same card produces the same due_at until review
///   metadata changes) and useful enough for "cards due today" /
///   "cards due this week" queries.
/// - `queue == 0` (new): `now + 365 days` placeholder — new cards
///   are "scheduled later" with no concrete date.
pub(crate) fn due_at_from_card(card: &CardInfo, now: DateTime<Utc>) -> DateTime<Utc> {
    match card.queue {
        1 => Utc
            .timestamp_opt(card.due, 0)
            .single()
            .unwrap_or(now),
        2 | 3 => now + ChronoDuration::days(card.interval.max(0) as i64),
        0 => now + ChronoDuration::days(365),
        _ => now,
    }
}

/// Tiny HTML-stripper for AnkiConnect's rendered `question` /
/// `answer` fields. Anki cards commonly use simple `<div>`/`<br>`
/// wrapping; we drop any `<…>` span and leave text. A full Anki
/// renderer is out of scope for a sync adapter.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Collapse internal runs of whitespace so `"<p>Front</p> <p>…</p>"`
    // doesn't land in the DB with embedded newlines/tabs.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Hot-reload watcher
// ---------------------------------------------------------------------------

/// Mirrors the Obsidian/Zotero config watchers — inotify on the sync
/// directory, call [`AnkiConnectAdapter::reload_config`] when
/// `ankiconnect.toml` changes, flip `enabled = false` on removal.
pub struct AnkiConnectConfigWatcher {
    _watcher: ConfigWatcher,
    task: JoinHandle<()>,
}

impl AnkiConnectConfigWatcher {
    pub fn spawn(
        adapter: Arc<AnkiConnectAdapter>,
        sync_dir: &Path,
    ) -> std::result::Result<Self, WatcherError> {
        let (watcher, mut rx) = watch_config_dir(sync_dir)?;
        let config_path = sync_dir.join("ankiconnect.toml");
        let task = tokio::spawn(async move {
            while let Some(change) = rx.recv().await {
                match change {
                    ConfigChange::Upserted(path) if path == config_path => {
                        match AnkiConnectConfig::load_from(&path) {
                            Ok(new) => {
                                tracing::info!(
                                    endpoint = %new.endpoint,
                                    enabled = new.enabled,
                                    poll_secs = new.poll_interval_secs,
                                    "ankiconnect hot-reload: applying new config"
                                );
                                adapter.reload_config(new);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "ankiconnect hot-reload: parse failed; adapter unchanged"
                                );
                            }
                        }
                    }
                    ConfigChange::Removed(path) if path == config_path => {
                        tracing::info!(
                            "ankiconnect hot-reload: config file removed; disabling adapter"
                        );
                        let mut current = adapter.current_config();
                        current.enabled = false;
                        adapter.reload_config(current);
                    }
                    _ => {}
                }
            }
            tracing::debug!("ankiconnect hot-reload: watcher channel closed");
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

impl std::fmt::Debug for AnkiConnectConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnkiConnectConfigWatcher").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_card(id: i64, note: i64, queue: i32, interval: i32, modified: i64) -> CardInfo {
        serde_json::from_value(json!({
            "cardId": id,
            "note": note,
            "deckName": "Default",
            "question": "<p>front</p>",
            "answer": "<p>back</p>",
            "interval": interval,
            "factor": 2500,
            "queue": queue,
            "mod": modified,
            "reps": 0,
            "due": 0,
        }))
        .unwrap()
    }

    #[test]
    fn strip_html_drops_tags_and_normalizes_whitespace() {
        assert_eq!(strip_html("<p>Hello</p>"), "Hello");
        assert_eq!(strip_html("<p>Hello</p>  <p>World</p>"), "Hello World");
        assert_eq!(strip_html("Plain text"), "Plain text");
        assert_eq!(strip_html("<br/><i>x</i>"), "x");
    }

    #[test]
    fn due_at_for_learning_queue_uses_unix_timestamp() {
        let mut card = make_card(1, 2, 1, 0, 0);
        // queue == 1; due is unix seconds.
        card.due = 1_700_000_000;
        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let due_at = due_at_from_card(&card, now);
        assert_eq!(due_at.timestamp(), 1_700_000_000);
    }

    #[test]
    fn due_at_for_review_queue_is_now_plus_interval_days() {
        let card = make_card(1, 2, 2, 7, 0);
        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let due = due_at_from_card(&card, now);
        assert_eq!(due, now + ChronoDuration::days(7));
    }

    #[test]
    fn due_at_for_new_card_is_far_future_placeholder() {
        let card = make_card(1, 2, 0, 0, 0);
        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let due = due_at_from_card(&card, now);
        assert_eq!(due, now + ChronoDuration::days(365));
    }

    #[test]
    fn suspended_card_is_not_syncable_sentinel() {
        let card = make_card(1, 2, -1, 0, 0);
        assert!(!card.is_syncable());
    }
}
