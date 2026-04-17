//! Warmup-mode module (spec §2.12.1).
//!
//! Subscribes to sway activity events and, on the first activity after
//! a long-enough gap, composes a [`WarmupPayload`] from:
//!
//! * Today's calendar events (via `DataStore::list_events` with a
//!   local-day window).
//! * The current count of due Anki flashcards (via
//!   `DataStore::list_flashcards` with `due_before = now`).
//! * Active projects (status != complete), newest-idle first, via
//!   [`ProjectRegistry`].
//!
//! The payload is pushed through the [`WidgetPublisher`] as
//! [`DaemonMessage::Warmup`]; the shell opens the overlay on receipt.
//! Dismissal is shell-local — the daemon doesn't track open-state.
//!
//! ## ctl force-fire
//!
//! `levshell-ctl warmup open` publishes [`Event::WarmupActionRequested`]
//! with `action = "open"`, which this module handles by bypassing the
//! gap check and firing immediately. Useful for development without
//! waiting 4 hours, and as the "foot in the door" for the eventual
//! MacOS-launcher-style palette entry (spec §2.1.2).

pub mod config;
pub mod persist;
pub mod trigger;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use levshell_core::{Event, EventKind, Module, ModuleError, ModuleResult};
use levshell_data::{DataStore, ListEvents, ListFlashcards, ListProjects, ProjectStatus};
use levshell_ipc::{
    DaemonMessage, WarmupEvent, WarmupPayload, WarmupProject, WidgetPublisher,
};
use levshell_projects::ProjectRegistry;
use tokio::sync::Mutex;

pub use config::{default_warmup_config_path, WarmupConfig, WarmupConfigError};
pub use persist::{default_warmup_state_path, PersistedWarmupState};
pub use trigger::TriggerState;

pub const MODULE_NAME: &str = "warmup";

pub struct WarmupModule {
    publisher: WidgetPublisher,
    store: DataStore,
    projects: Option<ProjectRegistry>,
    config: WarmupConfig,
    state_path: PathBuf,
    tracker: Arc<Mutex<TriggerState>>,
    last_warmup_at: Arc<Mutex<Option<DateTime<Utc>>>>,
}

impl WarmupModule {
    pub fn new(
        publisher: WidgetPublisher,
        store: DataStore,
        projects: Option<ProjectRegistry>,
    ) -> Self {
        Self::with_config(
            publisher,
            store,
            projects,
            WarmupConfig::default(),
            default_warmup_state_path(),
        )
    }

    pub fn with_config(
        publisher: WidgetPublisher,
        store: DataStore,
        projects: Option<ProjectRegistry>,
        config: WarmupConfig,
        state_path: PathBuf,
    ) -> Self {
        let persisted = PersistedWarmupState::load(&state_path);
        Self {
            publisher,
            store,
            projects,
            config,
            state_path,
            tracker: Arc::new(Mutex::new(TriggerState::new())),
            last_warmup_at: Arc::new(Mutex::new(persisted.last_warmup_at)),
        }
    }

    /// Load config from `warmup.toml` in the given directory. Missing
    /// file → defaults. Parse failures log and fall through to defaults
    /// so a broken TOML never wedges the daemon.
    pub fn load_config_from_dir(dir: &std::path::Path) -> WarmupConfig {
        let path = dir.join("warmup.toml");
        if !path.exists() {
            return WarmupConfig::default();
        }
        match WarmupConfig::load_from(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "warmup config parse failed; using defaults",
                );
                WarmupConfig::default()
            }
        }
    }

    pub fn config(&self) -> &WarmupConfig {
        &self.config
    }

    async fn on_activity(&self) -> ModuleResult<()> {
        let now_instant = Instant::now();
        let now_utc = Utc::now();
        let last_warmup = *self.last_warmup_at.lock().await;

        let should_fire = {
            let mut tracker = self.tracker.lock().await;
            tracker.decide(now_instant, now_utc, last_warmup, &self.config)
        };
        if should_fire {
            self.fire(now_utc).await?;
        }
        Ok(())
    }

    async fn on_ctl_request(&self, action: &str) -> ModuleResult<()> {
        match action {
            "open" => self.fire(Utc::now()).await,
            other => {
                tracing::warn!(action = other, "warmup: unknown ctl action, ignoring");
                Ok(())
            }
        }
    }

    async fn fire(&self, now_utc: DateTime<Utc>) -> ModuleResult<()> {
        let payload = self.build_payload(now_utc).await.map_err(|e| {
            ModuleError::failed(format!("warmup: payload assembly failed: {e}"))
        })?;

        let msg = DaemonMessage::Warmup(Box::new(payload));
        if let Err(e) = self.publisher.try_send(msg) {
            tracing::warn!(error = %e, "warmup: publish drop (channel full or closed)");
        }

        // Stamp the new last-warmup-at and persist. Done after publish
        // so a publish failure still advances the clock — we'd rather
        // skip this interval than keep re-firing.
        *self.last_warmup_at.lock().await = Some(now_utc);
        let persisted = PersistedWarmupState {
            last_warmup_at: Some(now_utc),
        };
        let path = self.state_path.clone();
        tokio::task::spawn_blocking(move || persisted.save(&path))
            .await
            .map_err(|e| ModuleError::failed(format!("warmup: persist join: {e}")))?;

        tracing::info!(module = MODULE_NAME, at = %now_utc, "warmup fired");
        Ok(())
    }

    async fn build_payload(
        &self,
        now_utc: DateTime<Utc>,
    ) -> Result<WarmupPayload, levshell_data::DataError> {
        let (start_of_day, end_of_day) = today_bounds_utc(now_utc);

        let events = self
            .store
            .list_events(ListEvents {
                after: Some(start_of_day),
                before: Some(end_of_day),
                limit: Some(32),
                ..Default::default()
            })
            .await?;

        let events: Vec<WarmupEvent> = events
            .into_iter()
            .map(|e| WarmupEvent {
                title: e.title,
                start_at: e.start_at.to_rfc3339(),
                end_at: e.end_at.to_rfc3339(),
                location: e.location,
            })
            .collect();

        let due_flashcards = self
            .store
            .list_flashcards(ListFlashcards {
                due_before: Some(now_utc),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let anki_due_count = due_flashcards.len() as u32;

        let projects = self.active_projects(now_utc).await?;

        Ok(WarmupPayload {
            fired_at: now_utc.to_rfc3339(),
            events,
            anki_due_count,
            projects,
        })
    }

    async fn active_projects(
        &self,
        now_utc: DateTime<Utc>,
    ) -> Result<Vec<WarmupProject>, levshell_data::DataError> {
        // If the ProjectRegistry is present we prefer it — it carries
        // runtime data (last-active timestamps) the raw data-store row
        // doesn't. Without a registry we fall back to raw rows sans
        // idle_secs.
        if let Some(registry) = &self.projects {
            let entries = registry.list().await;
            let mut out: Vec<WarmupProject> = entries
                .into_iter()
                .filter(|e| e.project.status != ProjectStatus::Complete)
                .map(|e| {
                    let idle_secs = e.runtime.last_active_at.map(|t| {
                        now_utc
                            .signed_duration_since(t)
                            .num_seconds()
                            .max(0) as u64
                    });
                    WarmupProject {
                        name: e.project.name,
                        status: e.project.status.as_str().to_owned(),
                        idle_secs,
                    }
                })
                .collect();
            // Most recently active first; projects never focused this
            // session go to the bottom.
            out.sort_by(|a, b| match (a.idle_secs, b.idle_secs) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            });
            return Ok(out);
        }

        let rows = self
            .store
            .list_projects(ListProjects {
                limit: Some(64),
                ..Default::default()
            })
            .await?;
        Ok(rows
            .into_iter()
            .filter(|p| p.status != ProjectStatus::Complete)
            .map(|p| WarmupProject {
                name: p.name,
                status: p.status.as_str().to_owned(),
                idle_secs: None,
            })
            .collect())
    }
}

/// UTC start and end of the local calendar day containing `now`. Used
/// to scope the events section of the warmup payload to "today in the
/// user's timezone".
fn today_bounds_utc(now_utc: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let local = now_utc.with_timezone(&Local);
    let start_local = local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time");
    let end_local = local
        .date_naive()
        .and_hms_opt(23, 59, 59)
        .expect("23:59:59 is a valid time");
    let start_utc = Local
        .from_local_datetime(&start_local)
        .earliest()
        .unwrap_or(local)
        .with_timezone(&Utc);
    let end_utc = Local
        .from_local_datetime(&end_local)
        .earliest()
        .unwrap_or(local + Duration::hours(24))
        .with_timezone(&Utc);
    (start_utc, end_utc)
}

#[async_trait]
impl Module for WarmupModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::WorkspaceChanged,
            EventKind::WindowFocused,
            EventKind::WarmupActionRequested,
        ]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        tracing::info!(
            module = MODULE_NAME,
            gap_secs = self.config.gap_secs,
            calendar_day_trigger = self.config.calendar_day_trigger,
            "warmup module started",
        );
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::WorkspaceChanged { .. } | Event::WindowFocused { .. } => {
                self.on_activity().await
            }
            Event::WarmupActionRequested { action } => self.on_ctl_request(action).await,
            _ => Ok(()),
        }
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_bounds_round_trip_to_near_24h() {
        let now = Utc.with_ymd_and_hms(2026, 4, 17, 14, 0, 0).unwrap();
        let (start, end) = today_bounds_utc(now);
        let span = end.signed_duration_since(start);
        // Within 25h handles timezones with half-hour offsets / DST
        // edges; key assertion is "a day, roughly".
        assert!(span.num_seconds() > 23 * 3600);
        assert!(span.num_seconds() < 25 * 3600);
        assert!(start <= now && now <= end);
    }
}
