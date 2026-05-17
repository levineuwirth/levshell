//! Clock / calendar hub feed (spec §2.1.5).
//!
//! The clock *widget* itself is shell-local (it ticks its own wall
//! clock — no daemon round-trip per second). This module supplies the
//! data the **dropdown hub** needs that the shell cannot synthesize:
//! upcoming events from the unified store (populated by the CalDAV sync
//! adapter, or any other event source). It polls `DataStore::list_events`
//! on a slow cadence and publishes a [`ClockHubPayload`]; the shell
//! renders the upcoming list and derives the next-event countdown.
//!
//! With no events in the store this publishes an empty list every tick —
//! cheap, and it lets the shell distinguish "no upcoming events" from
//! "never received data". Mirrors the warmup module's event projection
//! (`Event` -> `WarmupEvent`), which is intentionally the same shape.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use levshell_core::{Module, ModuleResult};
use levshell_data::{DataStore, ListEvents};
use levshell_ipc::{ClockHubPayload, DaemonMessage, WarmupEvent, WidgetPublisher};

const MODULE_NAME: &str = "clock";

/// How often the upcoming-events snapshot is refreshed. Calendar data
/// changes on the order of minutes (CalDAV sync cadence), so a tight
/// poll would be wasted work; 60s keeps the countdown reasonably fresh.
const TICK_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// How far ahead to surface events. A week is enough for the dropdown's
/// "upcoming" list without turning it into a full agenda view.
const LOOKAHEAD_DAYS: i64 = 7;

/// Max events per snapshot — bounds payload size and matches what the
/// dropdown can usefully show.
const MAX_EVENTS: u32 = 12;

pub struct ClockModule {
    store: DataStore,
    publisher: WidgetPublisher,
}

impl ClockModule {
    pub fn new(store: DataStore, publisher: WidgetPublisher) -> Self {
        Self { store, publisher }
    }

    /// Query upcoming events and push a fresh [`ClockHubPayload`]. A
    /// store error is logged and swallowed — a transient DB hiccup must
    /// not park the module; the next tick retries.
    async fn refresh(&self) {
        let now = Utc::now();
        let events = match self
            .store
            .list_events(ListEvents {
                // `after` filters `end_at >= now` (ongoing events still
                // count); `before` filters `start_at <= now+lookahead`.
                after: Some(now),
                before: Some(now + Duration::days(LOOKAHEAD_DAYS)),
                limit: Some(MAX_EVENTS),
                ..Default::default()
            })
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "clock: list_events failed; skipping tick");
                return;
            }
        };

        // `list_events` already returns ORDER BY start_at ASC.
        let events: Vec<WarmupEvent> = events
            .into_iter()
            .map(|e| WarmupEvent {
                title: e.title,
                start_at: e.start_at.to_rfc3339(),
                end_at: e.end_at.to_rfc3339(),
                location: e.location,
            })
            .collect();

        let payload = ClockHubPayload {
            generated_at: now.to_rfc3339(),
            events,
        };
        if let Err(e) = self
            .publisher
            .try_send(DaemonMessage::ClockHub(Box::new(payload)))
        {
            tracing::warn!(error = %e, "clock: publish drop (channel full or closed)");
        }
    }
}

#[async_trait]
impl Module for ClockModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(TICK_INTERVAL)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // Publish immediately so the dropdown has data before the first
        // tick interval elapses.
        self.refresh().await;
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.refresh().await;
        Ok(())
    }
}
