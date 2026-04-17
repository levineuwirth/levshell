//! Interruption-cost module (spec §2.12.3).
//!
//! When the user returns to a workspace they've been away from for more
//! than [`MIN_AWAY_SECS`], publish a widget state containing the
//! away-time and a `shown_at_ms` tripwire. The QML widget renders a
//! subtle pill that fades in and auto-hides after a few seconds. The
//! goal is non-punitive awareness of re-entry cost, not a nag.
//!
//! The module listens on the bus for [`Event::WorkspaceChanged`], which
//! the sway module publishes on every `WorkspaceChange::Focus`. It
//! maintains an in-memory `HashMap<workspace_name, Instant>` marking
//! when each workspace was last left (i.e. un-focused). On re-entry
//! it computes `now - last_left_at`; if it exceeds the threshold, it
//! publishes a widget update.
//!
//! ## Why in-memory only
//!
//! State resets on daemon restart. Spec §2.12.3 is about live
//! awareness during a session — there's no value to reporting "you've
//! been away from this workspace since last Tuesday". A fresh daemon
//! simply starts the clock on each workspace as it's first observed.
//!
//! ## Why the display timer lives in QML
//!
//! The daemon fires one `WidgetUpdate` per interruption and forgets.
//! The QML widget handles fade-in / fade-out based on the `shown_at_ms`
//! wall-clock timestamp. Keeping the fade timer in QML means the bar
//! can drop / replay an update without the daemon having to track
//! display state.
//!
//! [`Event::WorkspaceChanged`]: levshell_core::Event

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::Utc;
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

pub const MODULE_NAME: &str = "interruption-cost";
pub const WIDGET_ID: &str = "interruption-cost";
pub const WIDGET_TYPE: &str = "interruption_cost";

/// Default: below 2 minutes is "quick flip", not worth surfacing.
pub const MIN_AWAY_SECS: u64 = 120;

/// The widget state payload. Serialized straight into the
/// `WidgetUpdate.state` blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptionState {
    /// The workspace the user just re-entered.
    pub workspace: String,
    /// How long the user was gone, in seconds. Always >= the configured
    /// threshold.
    pub away_seconds: u64,
    /// Wall-clock millisecond timestamp at which the daemon fired this
    /// update. The QML side binds animation start to changes in this
    /// field so two back-to-back interruptions both animate.
    pub shown_at_ms: i64,
}

/// The "nothing to show" state. Sent on startup so the shell has a
/// rendered-but-empty widget from first frame.
fn empty_state() -> InterruptionState {
    InterruptionState {
        workspace: String::new(),
        away_seconds: 0,
        shown_at_ms: 0,
    }
}

/// Pure state machine. Kept separate from the [`Module`] so tests can
/// drive arbitrary `Instant` timelines without an async runtime.
#[derive(Debug, Default)]
pub struct InterruptionTracker {
    min_away_secs: u64,
    current_workspace: Option<String>,
    last_left_at: HashMap<String, Instant>,
}

impl InterruptionTracker {
    pub fn new(min_away_secs: u64) -> Self {
        Self {
            min_away_secs,
            current_workspace: None,
            last_left_at: HashMap::new(),
        }
    }

    /// Apply a workspace-focus change. Returns the state to publish, or
    /// `None` if nothing changed (e.g. a non-focus WorkspaceChanged
    /// fired for the same workspace, a first-ever focus, or the
    /// away-time was below threshold).
    ///
    /// `shown_at_ms` is threaded in from the caller so tests can hold
    /// wall-clock time fixed; the [`Module`] passes [`now_ms`].
    pub fn apply_focus(
        &mut self,
        new_ws: &str,
        now: Instant,
        shown_at_ms: i64,
    ) -> Option<InterruptionState> {
        // Non-Focus WorkspaceChanged events (Init/Empty/Rename/etc.) can
        // fire with the same name. Ignore unless the name actually flipped.
        if self.current_workspace.as_deref() == Some(new_ws) {
            return None;
        }

        // Mark the outgoing workspace as just-left.
        if let Some(prev) = self.current_workspace.take() {
            self.last_left_at.insert(prev, now);
        }

        let away = self
            .last_left_at
            .get(new_ws)
            .map(|t| now.saturating_duration_since(*t));

        self.current_workspace = Some(new_ws.to_owned());

        let away = away?;
        if away < Duration::from_secs(self.min_away_secs) {
            return None;
        }

        Some(InterruptionState {
            workspace: new_ws.to_owned(),
            away_seconds: away.as_secs(),
            shown_at_ms,
        })
    }
}

pub struct InterruptionCostModule {
    publisher: WidgetPublisher,
    tracker: InterruptionTracker,
}

impl InterruptionCostModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            tracker: InterruptionTracker::new(MIN_AWAY_SECS),
        }
    }

    pub fn with_min_away_secs(mut self, secs: u64) -> Self {
        self.tracker.min_away_secs = secs;
        self
    }

    fn publish(&self, state: InterruptionState) {
        let msg = DaemonMessage::WidgetUpdate(WidgetUpdate {
            widget_id: WIDGET_ID.to_owned(),
            widget_type: WIDGET_TYPE.to_owned(),
            state: serde_json::to_value(&state).unwrap_or(serde_json::Value::Null),
            status: WidgetStatus::Normal,
        });
        if let Err(e) = self.publisher.try_send(msg) {
            tracing::warn!(error = %e, "interruption-cost: widget update drop");
        }
    }
}

#[async_trait]
impl Module for InterruptionCostModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: WIDGET_ID.into(),
            widget_type: WIDGET_TYPE.into(),
        }]
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WorkspaceChanged]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // Seed an empty state so the shell has a rendered widget the
        // moment it connects — avoids a flicker when the first real
        // interruption fires.
        self.publish(empty_state());
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if let Event::WorkspaceChanged { name, .. } = event {
            if let Some(state) = self.tracker.apply_focus(name, Instant::now(), now_ms()) {
                self.publish(state);
            }
        }
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

fn now_ms() -> i64 {
    // SystemTime is monotonic-unsafe but this is a display tripwire, not
    // a timing primitive; a clock jump just means the fade might misbehave
    // once.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|_| Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant_plus(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn tracker(min_away: u64) -> InterruptionTracker {
        InterruptionTracker::new(min_away)
    }

    #[test]
    fn first_focus_yields_no_notification() {
        let mut t = tracker(60);
        let t0 = Instant::now();
        assert!(t.apply_focus("1:code", t0, 1).is_none());
    }

    #[test]
    fn immediate_return_below_threshold_is_silent() {
        let mut t = tracker(60);
        let t0 = Instant::now();
        t.apply_focus("1:code", t0, 1);
        t.apply_focus("2:docs", instant_plus(t0, 5), 2);
        // 10 seconds total away from "1:code" — under 60s threshold.
        assert!(t.apply_focus("1:code", instant_plus(t0, 15), 3).is_none());
    }

    #[test]
    fn long_return_above_threshold_fires() {
        let mut t = tracker(60);
        let t0 = Instant::now();
        t.apply_focus("1:code", t0, 1);
        t.apply_focus("2:docs", instant_plus(t0, 5), 2);
        let state = t
            .apply_focus("1:code", instant_plus(t0, 605), 3)
            .expect("should fire");
        assert_eq!(state.workspace, "1:code");
        assert_eq!(state.away_seconds, 600);
        assert_eq!(state.shown_at_ms, 3);
    }

    #[test]
    fn same_workspace_reported_twice_is_ignored() {
        // Sway emits WorkspaceChanged for Init/Empty/Rename even when the
        // focused name hasn't changed. Those must not reset the clock.
        let mut t = tracker(60);
        let t0 = Instant::now();
        t.apply_focus("1:code", t0, 1);
        assert!(t.apply_focus("1:code", instant_plus(t0, 100), 2).is_none());
        assert_eq!(t.current_workspace.as_deref(), Some("1:code"));
        assert!(!t.last_left_at.contains_key("1:code"));
    }

    #[test]
    fn multiple_workspaces_tracked_independently() {
        let mut t = tracker(60);
        let t0 = Instant::now();
        t.apply_focus("a", t0, 1);
        t.apply_focus("b", instant_plus(t0, 10), 2);
        t.apply_focus("c", instant_plus(t0, 20), 3);
        let sa = t
            .apply_focus("a", instant_plus(t0, 210), 4)
            .expect("a should fire");
        assert_eq!(sa.workspace, "a");
        assert_eq!(sa.away_seconds, 200);
        let sb = t
            .apply_focus("b", instant_plus(t0, 220), 5)
            .expect("b should fire");
        assert_eq!(sb.workspace, "b");
        assert_eq!(sb.away_seconds, 200);
    }

    #[test]
    fn round_trip_under_threshold_then_over() {
        let mut t = tracker(60);
        let t0 = Instant::now();
        t.apply_focus("a", t0, 1);
        t.apply_focus("b", instant_plus(t0, 1), 2);
        assert!(t.apply_focus("a", instant_plus(t0, 30), 3).is_none());
        t.apply_focus("b", instant_plus(t0, 31), 4);
        let s = t
            .apply_focus("a", instant_plus(t0, 500), 5)
            .expect("should fire");
        assert_eq!(s.away_seconds, 469);
    }
}
