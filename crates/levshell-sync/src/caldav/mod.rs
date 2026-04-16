//! CalDAV sync adapter (§5.2, §6.2 item 2).
//!
//! Fourth and final v1 sync adapter. Populates the `events` table
//! from one or more CalDAV collections. Per tick:
//!
//! 1. For each configured `[[calendar]]`: PROPFIND depth=1 to list
//!    every `(href, etag)` pair.
//! 2. Diff against `sync_metadata` keyed on
//!    `(provider="caldav", external_id="<calendar_name>/<uid>")`.
//!    New hrefs and hrefs with a changed ETag get fetched via GET;
//!    unchanged ones are skipped.
//! 3. Each fetched `.ics` is translated by [`translator::translate`]
//!    into a [`translator::TranslatedEvent`] and upserted into the
//!    store.
//! 4. Any sync_metadata row whose external_id is not in the current
//!    fleet (all calendars combined) gets its Event deleted.
//!
//! The adapter is **import-only** in v1 — PUT/DELETE writeback and
//! OAuth auth land in a later pass. See the module docs on
//! [`translator`] for the recurrence/timezone caveats.

pub mod client;
pub mod config;
pub mod translator;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_config::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
use levshell_data::{EntityType, EventPatch, NewEvent, SyncDirection, SyncMetadata};
use tokio::task::JoinHandle;

use crate::adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub use client::{CalDavClient, CalDavError, CalDavHttpClient, DavEntry};
pub use config::{CalDavConfig, CalDavConfigError, CalendarSource};
pub use translator::{translate, TranslateError, TranslatedEvent};

/// Provider name. Persisted in `sync_metadata.provider`.
pub const PROVIDER_NAME: &str = "caldav";

impl From<CalDavError> for SyncError {
    fn from(e: CalDavError) -> Self {
        match e {
            CalDavError::Http { .. } => SyncError::Unavailable(e.to_string()),
            _ => SyncError::External(e.to_string()),
        }
    }
}

impl From<TranslateError> for SyncError {
    fn from(e: TranslateError) -> Self {
        SyncError::External(e.to_string())
    }
}

impl From<CalDavConfigError> for SyncError {
    fn from(e: CalDavConfigError) -> Self {
        SyncError::External(e.to_string())
    }
}

/// Per-calendar client bundle. The adapter builds one of these at
/// startup (after resolving `password_command`) and rebuilds when
/// the config hot-reloads.
struct ResolvedCalendar {
    source: CalendarSource,
    client: Arc<dyn CalDavClient>,
}

/// Factory the adapter calls to build a CalDAV client. Production
/// wires this to [`CalDavHttpClient::new`]; tests plug in a mock
/// factory so each calendar can present canned responses.
pub type ClientFactory = Arc<
    dyn Fn(&CalendarSource, Duration) -> std::result::Result<Arc<dyn CalDavClient>, CalDavError>
        + Send
        + Sync,
>;

pub struct CalDavAdapter {
    config: Arc<RwLock<CalDavConfig>>,
    /// Currently-live per-calendar clients, one per `[[calendar]]`.
    /// Rebuilt on hot-reload by [`Self::reload_config`].
    calendars: Arc<RwLock<Vec<ResolvedCalendar>>>,
    factory: ClientFactory,
}

impl CalDavAdapter {
    /// Production constructor. Resolves passwords, builds an HTTP
    /// client per calendar, and wires up. Per-calendar resolution
    /// failures are logged and skipped so one bad credential doesn't
    /// take down the adapter.
    pub fn new(config: CalDavConfig) -> std::result::Result<Self, CalDavError> {
        let factory: ClientFactory = Arc::new(|source, timeout| {
            let password = source.resolve_password().map_err(|e| CalDavError::Malformed {
                url: source.url.clone(),
                reason: format!("password resolution failed: {e}"),
            })?;
            let c = CalDavHttpClient::new(&source.username, &password, timeout)?;
            Ok(Arc::new(c) as Arc<dyn CalDavClient>)
        });
        let calendars = build_calendars(&config, &factory);
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            calendars: Arc::new(RwLock::new(calendars)),
            factory,
        })
    }

    /// Build an adapter with an arbitrary client factory. Used by
    /// unit tests to plug in a [`CalDavClient`] mock.
    pub fn with_factory(config: CalDavConfig, factory: ClientFactory) -> Self {
        let calendars = build_calendars(&config, &factory);
        Self {
            config: Arc::new(RwLock::new(config)),
            calendars: Arc::new(RwLock::new(calendars)),
            factory,
        }
    }

    pub fn reload_config(&self, new: CalDavConfig) {
        let rebuilt = build_calendars(&new, &self.factory);
        {
            let mut guard = self.config.write().expect("caldav config lock poisoned");
            *guard = new;
        }
        let mut guard = self
            .calendars
            .write()
            .expect("caldav calendars lock poisoned");
        *guard = rebuilt;
    }

    pub fn current_config(&self) -> CalDavConfig {
        self.config.read().expect("caldav config lock poisoned").clone()
    }

    fn snapshot_calendars(&self) -> Vec<(CalendarSource, Arc<dyn CalDavClient>)> {
        self.calendars
            .read()
            .expect("caldav calendars lock poisoned")
            .iter()
            .map(|c| (c.source.clone(), c.client.clone()))
            .collect()
    }
}

fn build_calendars(config: &CalDavConfig, factory: &ClientFactory) -> Vec<ResolvedCalendar> {
    let mut out = Vec::with_capacity(config.calendars.len());
    for source in &config.calendars {
        match factory(source, config.request_timeout()) {
            Ok(client) => out.push(ResolvedCalendar {
                source: source.clone(),
                client,
            }),
            Err(e) => {
                tracing::warn!(
                    calendar = %source.name,
                    error = %e,
                    "caldav: skipping calendar — client construction failed"
                );
            }
        }
    }
    out
}

#[async_trait]
impl SyncAdapter for CalDavAdapter {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Event]
    }

    fn poll_interval(&self) -> Duration {
        self.current_config().poll_interval()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        if !self.current_config().enabled {
            return SyncStatus::Unavailable;
        }
        let calendars = self.snapshot_calendars();
        if calendars.is_empty() {
            return SyncStatus::Unavailable;
        }
        // Try the first calendar's PROPFIND. If even one responds
        // we're healthy — one auth failure elsewhere shouldn't
        // degrade the adapter globally.
        for (source, client) in &calendars {
            if client.list_entries(&source.url).await.is_ok() {
                return SyncStatus::Healthy;
            }
        }
        SyncStatus::Unavailable
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let config = self.current_config();
        if !config.enabled {
            return Ok(SyncReport::default());
        }

        let calendars = self.snapshot_calendars();
        if calendars.is_empty() {
            return Ok(SyncReport::default());
        }

        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external_id: HashMap<String, SyncMetadata> = existing
            .into_iter()
            .map(|m| (m.external_id.clone(), m))
            .collect();

        let mut report = SyncReport::default();

        for (source, client) in &calendars {
            let entries = match client.list_entries(&source.url).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        calendar = %source.name,
                        error = %e,
                        "caldav: skipping calendar this tick — PROPFIND failed"
                    );
                    continue;
                }
            };

            for entry in entries {
                let abs_url = absolutize_href(&source.url, &entry.href);

                // We don't have the UID yet — the external_id key
                // depends on parsing the ICS. So we can't diff by
                // external_id before GET unless we store the etag by
                // href. v1 keeps it simple: fetch the ICS, get the
                // UID, then look up sync_metadata by
                // `"{calendar_name}/{uid}"`. For unchanged events
                // the etag-vs-sync_hash comparison still short-circuits
                // upsert work to a hash-only check — server still
                // has to hand us the body, which is fine on
                // Nextcloud's scale.
                //
                // A future optimization: store the etag-by-href map
                // across ticks so unchanged entries skip the GET.
                let ics = match client.fetch_ics(&abs_url).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            calendar = %source.name,
                            href = %entry.href,
                            error = %e,
                            "caldav: GET failed; skipping entry"
                        );
                        continue;
                    }
                };

                let translated = match translate(&ics) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            calendar = %source.name,
                            href = %entry.href,
                            error = %e,
                            "caldav: ICS parse failed; skipping entry"
                        );
                        continue;
                    }
                };

                // A single .ics usually contains one VEVENT, but
                // recurring events can split into base + override
                // entries. v1 keeps them all — each gets its own
                // external_id via UID (overrides share the base
                // UID, which means last-override-wins within one
                // .ics — acceptable for v1).
                for event in translated {
                    let external_id = format!("{}/{}", source.name, event.uid);
                    let meta = by_external_id.remove(&external_id);
                    match meta {
                        None => apply_insert(ctx, &external_id, &event, &entry, &mut report)
                            .await?,
                        Some(prev) => {
                            if prev.sync_hash.as_deref() == Some(entry.etag.as_str()) {
                                continue;
                            }
                            apply_update(
                                ctx,
                                &external_id,
                                &event,
                                &entry,
                                &prev,
                                &mut report,
                            )
                            .await?;
                        }
                    }
                }
            }
        }

        // Deletions: anything left in by_external_id isn't on any of
        // the configured calendars anymore.
        for (_, stale) in by_external_id {
            apply_delete(ctx, &stale, &mut report).await?;
        }

        Ok(report)
    }
}

async fn apply_insert(
    ctx: &SyncContext,
    external_id: &str,
    event: &TranslatedEvent,
    entry: &DavEntry,
    report: &mut SyncReport,
) -> Result<()> {
    let new = NewEvent {
        title: event.summary.clone(),
        start_at: event.start_at,
        end_at: event.end_at,
        location: event.location.clone(),
        description: event.description.clone(),
        url: event.url.clone(),
        project_id: None,
        recurrence: event.recurrence.clone(),
        reminders: Vec::new(),
    };
    let inserted = ctx.store.insert_event(new).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: inserted.id,
            entity_type: EntityType::Event,
            provider: PROVIDER_NAME.into(),
            external_id: external_id.to_string(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(entry.etag.clone()),
        })
        .await?;
    report.upserted += 1;
    Ok(())
}

async fn apply_update(
    ctx: &SyncContext,
    external_id: &str,
    event: &TranslatedEvent,
    entry: &DavEntry,
    prev: &SyncMetadata,
    report: &mut SyncReport,
) -> Result<()> {
    let Some(existing) = ctx.store.get_event(prev.entity_id).await? else {
        return apply_insert(ctx, external_id, event, entry, report).await;
    };

    if existing.updated_at > prev.last_synced_at {
        report.conflicts.push(SyncConflict {
            entity_type: EntityType::Event,
            external_id: external_id.to_string(),
            reason: "local event modified since last sync; external copy wins (v1)".into(),
        });
    }

    let patch = EventPatch {
        title: Some(event.summary.clone()),
        start_at: Some(event.start_at),
        end_at: Some(event.end_at),
        location: Some(event.location.clone()),
        description: Some(event.description.clone()),
        url: Some(event.url.clone()),
        recurrence: Some(event.recurrence.clone()),
        ..Default::default()
    };
    ctx.store.update_event(existing.id, patch).await?;
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id: existing.id,
            entity_type: EntityType::Event,
            provider: PROVIDER_NAME.into(),
            external_id: external_id.to_string(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(entry.etag.clone()),
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
    ctx.store.delete_event(stale.entity_id).await?;
    ctx.store
        .clear_sync_metadata(stale.entity_id, EntityType::Event, PROVIDER_NAME)
        .await?;
    report.deleted += 1;
    Ok(())
}

/// Resolve a PROPFIND `<d:href>` against the calendar's base URL.
/// The server may return an absolute URL (`https://srv/dav/x.ics`)
/// or a path (`/dav/x.ics`). We normalize to absolute using the
/// base's scheme + authority.
pub fn absolutize_href(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Ok(parsed_base) = reqwest::Url::parse(base) {
        if let Ok(joined) = parsed_base.join(href) {
            return joined.to_string();
        }
    }
    // Fallback: concat with / handling.
    if let Some(root_end) = base
        .match_indices('/')
        .nth(2)
        .map(|(i, _)| i)
    {
        let root = &base[..root_end];
        if let Some(stripped) = href.strip_prefix('/') {
            return format!("{root}/{stripped}");
        }
    }
    href.to_string()
}

// ---------------------------------------------------------------------------
// Hot-reload watcher
// ---------------------------------------------------------------------------

pub struct CalDavConfigWatcher {
    _watcher: ConfigWatcher,
    task: JoinHandle<()>,
}

impl CalDavConfigWatcher {
    pub fn spawn(
        adapter: Arc<CalDavAdapter>,
        sync_dir: &Path,
    ) -> std::result::Result<Self, WatcherError> {
        let (watcher, mut rx) = watch_config_dir(sync_dir)?;
        let config_path = sync_dir.join("caldav.toml");
        let task = tokio::spawn(async move {
            while let Some(change) = rx.recv().await {
                match change {
                    ConfigChange::Upserted(path) if path == config_path => {
                        match CalDavConfig::load_from(&path) {
                            Ok(new) => {
                                tracing::info!(
                                    enabled = new.enabled,
                                    calendars = new.calendars.len(),
                                    poll_secs = new.poll_interval_secs,
                                    "caldav hot-reload: applying new config"
                                );
                                adapter.reload_config(new);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "caldav hot-reload: parse failed; adapter unchanged"
                                );
                            }
                        }
                    }
                    ConfigChange::Removed(path) if path == config_path => {
                        tracing::info!(
                            "caldav hot-reload: config file removed; disabling adapter"
                        );
                        let mut current = adapter.current_config();
                        current.enabled = false;
                        adapter.reload_config(current);
                    }
                    _ => {}
                }
            }
            tracing::debug!("caldav hot-reload: watcher channel closed");
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

impl std::fmt::Debug for CalDavConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavConfigWatcher").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolutize_preserves_absolute_href() {
        assert_eq!(
            absolutize_href("https://srv/cal/", "https://srv/cal/x.ics"),
            "https://srv/cal/x.ics"
        );
    }

    #[test]
    fn absolutize_joins_path_relative_href() {
        assert_eq!(
            absolutize_href("https://srv/dav/cal/", "/dav/cal/x.ics"),
            "https://srv/dav/cal/x.ics"
        );
    }

    #[test]
    fn absolutize_joins_filename_only() {
        assert_eq!(
            absolutize_href("https://srv/dav/cal/", "x.ics"),
            "https://srv/dav/cal/x.ics"
        );
    }
}
