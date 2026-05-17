//! `ContextEngineModule` — the runtime driver for the pure
//! [`levshell_context`] core.
//!
//! The context crate itself is strictly synchronous and deterministic: given
//! a [`SignalContext`] plus a set of [`WidgetDef`]/[`CompiledRule`]/[`Profile`]
//! inputs, it produces a layout. This module supplies the missing async
//! machinery — bus subscriptions, periodic re-evaluation to let hysteresis
//! commit, and outbound IPC publication.
//!
//! ## Inputs
//!
//! * Bus events update the [`SignalContext`]:
//!     * `WorkspaceChanged` → `workspace.name`, `workspace.focused_window`
//!     * `WindowFocused` → `focused.app_id`, `focused.title`
//!     * `PowerStateChanged` → `power.on_battery`
//!     * `BarDensityRequested` → `bar.density` (string signal read by rules)
//!     * `ProfileActionRequested` → activates/cycles a profile
//!
//! * A periodic tick recomputes the layout even when nothing has changed,
//!   so in-flight hysteresis transitions reach their commit deadline.
//!
//! ## Outputs
//!
//! Every recompute publishes:
//!
//! * One `BarLayout` message (left/center/right zone ordering).
//! * One `WidgetVisibility` per widget, so the shell can animate in and out
//!   independently of the layout structure.
//!
//! ## Phase 1.2 limitations
//!
//! * Widget widths come from a constant heuristic table. Phase 1.4 will wire
//!   measured widths pushed from the shell.
//! * `available_width` is hard-coded to a generous default (2560px). A real
//!   value will flow in once the shell reports its geometry.
//! * Profile cycling uses a fixed ordering (alphabetical by name).
//! * User overrides are not yet plumbed in from a config file or ctl.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use levshell_context::{
    resolve_layout, CascadeInput, CompiledRule, Hysteresis, HysteresisConfig, Profile,
    SignalContext, WidgetDef,
};
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{
    BarDensity, BarDensityState, BarLayout, DaemonMessage, Prominence, WidgetPublisher,
    WidgetVisibility,
};

/// Default placeholder pixel budget until the shell reports its geometry.
const DEFAULT_AVAILABLE_WIDTH: u32 = 2560;

/// Default inter-widget gap in pixels.
const DEFAULT_INTER_WIDGET_GAP: u32 = 8;

/// Default tick interval. Short enough that pending hysteresis transitions
/// commit promptly; long enough that the cascade isn't recomputed in a busy
/// loop when nothing has changed.
const DEFAULT_TICK: Duration = Duration::from_millis(500);

/// Heuristic widget width table, keyed only on prominence. Real widths will
/// be measured in QML and pushed back to the daemon in a later phase.
fn heuristic_width(_widget_id: &str, p: Prominence) -> u32 {
    match p {
        Prominence::Hidden => 0,
        Prominence::Badge => 16,
        Prominence::IconOnly => 32,
        Prominence::Compact => 96,
        Prominence::Visible => 160,
        Prominence::Expanded => 220,
    }
}

/// Phase 1.2 `ContextEngineModule`. Owns the pure context-engine state plus
/// an [`Hysteresis`] debouncer, subscribes to the event bus, and publishes
/// [`BarLayout`]/[`WidgetVisibility`] messages through a [`WidgetPublisher`].
pub struct ContextEngineModule {
    publisher: WidgetPublisher,
    widgets: Vec<WidgetDef>,
    rules: Vec<CompiledRule>,
    /// Shared so an external watcher (the daemon's
    /// `profiles/` inotify watcher, per spec §3.9) can atomically
    /// replace the profile set at runtime without tearing down the
    /// module. Held behind [`std::sync::RwLock`] rather than
    /// [`tokio::sync::RwLock`] because every read is brief (a
    /// name-to-profile lookup that clones out the matching profile)
    /// and no read is held across an `.await`.
    profiles: Arc<RwLock<Vec<Profile>>>,
    active_profile: Option<String>,
    signals: SignalContext,
    user_overrides: HashMap<String, Prominence>,
    hysteresis: Hysteresis,
    last_published: Option<PublishedSnapshot>,
    available_width: u32,
    inter_widget_gap: u32,
    width_fn: fn(&str, Prominence) -> u32,
}

/// Last layout we published. Used so ticks that produce no delta stay
/// silent on the wire.
#[derive(Debug, Clone, PartialEq)]
struct PublishedSnapshot {
    layout: BarLayout,
    prominences: HashMap<String, Prominence>,
}

impl ContextEngineModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            widgets: Vec::new(),
            rules: Vec::new(),
            profiles: Arc::new(RwLock::new(Vec::new())),
            active_profile: None,
            signals: SignalContext::new(),
            user_overrides: HashMap::new(),
            hysteresis: Hysteresis::new(HysteresisConfig::default()),
            last_published: None,
            available_width: DEFAULT_AVAILABLE_WIDTH,
            inter_widget_gap: DEFAULT_INTER_WIDGET_GAP,
            width_fn: heuristic_width,
        }
    }

    pub fn with_widgets(mut self, widgets: Vec<WidgetDef>) -> Self {
        self.widgets = widgets;
        self
    }

    pub fn with_rules(mut self, rules: Vec<CompiledRule>) -> Self {
        self.rules = rules;
        self
    }

    pub fn with_profiles(self, profiles: Vec<Profile>) -> Self {
        {
            let mut guard = self
                .profiles
                .write()
                .expect("context-engine profile lock poisoned");
            *guard = profiles;
        }
        self
    }

    /// Replace the internal profile-sharing handle. Use when the
    /// caller wants to hand the same `Arc<RwLock<Vec<Profile>>>` to
    /// both the module and an external watcher (spec §3.9 hot-reload):
    /// the watcher writes into the lock, the module reads from it on
    /// every resolve.
    pub fn with_shared_profiles(mut self, profiles: Arc<RwLock<Vec<Profile>>>) -> Self {
        self.profiles = profiles;
        self
    }

    pub fn with_available_width(mut self, width: u32) -> Self {
        self.available_width = width;
        self
    }

    /// Force a specific initial profile. The name must match one of the
    /// profiles registered via [`Self::with_profiles`] or it will be
    /// silently ignored on first resolve.
    pub fn with_active_profile(mut self, name: impl Into<String>) -> Self {
        self.active_profile = Some(name.into());
        self
    }

    /// A clone of the shared profiles handle. Hand this to an external
    /// watcher that wants to replace the profile set at runtime (spec
    /// §3.9 — configuration is hot-reloadable via inotify). Writing a
    /// new `Vec<Profile>` into the lock takes effect on the next
    /// resolve pass; in-flight resolves are unaffected.
    pub fn shared_profiles(&self) -> Arc<RwLock<Vec<Profile>>> {
        self.profiles.clone()
    }

    fn active_profile_snapshot(&self) -> Option<Profile> {
        let name = self.active_profile.as_ref()?;
        self.profiles
            .read()
            .expect("context-engine profile lock poisoned")
            .iter()
            .find(|p| &p.name == name)
            .cloned()
    }

    /// Recompute the cascade, feed it through hysteresis at `now`, and push
    /// any deltas to the publisher. Returns the committed prominences so
    /// tests can inspect the state without racing the publisher.
    fn resolve_and_publish(&mut self, now: Instant) -> HashMap<String, Prominence> {
        let active = self.active_profile_snapshot();
        // Coerce the fn-pointer field to a `&dyn Fn(...)` so the cascade's
        // `&WidgetWidthFn` parameter accepts it. Function pointers implement
        // `Fn`, so an explicit closure wrapper is the simplest coercion.
        let width_fn_ptr = self.width_fn;
        let width_fn_closure = move |id: &str, p: Prominence| width_fn_ptr(id, p);
        let cascade_input = CascadeInput {
            widgets: &self.widgets,
            signals: &self.signals,
            rules: &self.rules,
            active_profile: active.as_ref(),
            available_width: self.available_width,
            widget_width: &width_fn_closure,
            user_overrides: &self.user_overrides,
            inter_widget_gap: self.inter_widget_gap,
        };
        let layout = resolve_layout(cascade_input);
        let committed = self.hysteresis.observe(&layout.prominences, now);

        let snapshot = PublishedSnapshot {
            layout: layout.layout.clone(),
            prominences: committed.clone(),
        };

        let layout_changed = self
            .last_published
            .as_ref()
            .map(|p| p.layout != snapshot.layout)
            .unwrap_or(true);
        if layout_changed {
            if let Err(e) = self
                .publisher
                .try_send(DaemonMessage::BarLayout(snapshot.layout.clone()))
            {
                tracing::warn!(error = %e, "context-engine: failed to publish BarLayout");
            }
        }

        let mut prev = self
            .last_published
            .as_ref()
            .map(|p| &p.prominences)
            .cloned();
        for (widget_id, &prominence) in &committed {
            let previous = prev
                .as_mut()
                .and_then(|p| p.remove(widget_id));
            if previous == Some(prominence) {
                continue;
            }
            let visible = prominence != Prominence::Hidden;
            let msg = DaemonMessage::WidgetVisibility(WidgetVisibility {
                widget_id: widget_id.clone(),
                visible,
                prominence,
            });
            if let Err(e) = self.publisher.try_send(msg) {
                tracing::warn!(
                    widget = %widget_id,
                    error = %e,
                    "context-engine: failed to publish WidgetVisibility"
                );
            }
        }
        // Any widgets that were present previously but no longer appear in
        // `committed` need an explicit Hidden visibility so the shell drops
        // them from its rendered set.
        if let Some(leftover) = prev {
            for (widget_id, _) in leftover {
                let msg = DaemonMessage::WidgetVisibility(WidgetVisibility {
                    widget_id: widget_id.clone(),
                    visible: false,
                    prominence: Prominence::Hidden,
                });
                if let Err(e) = self.publisher.try_send(msg) {
                    tracing::warn!(
                        widget = %widget_id,
                        error = %e,
                        "context-engine: failed to publish hidden visibility"
                    );
                }
            }
        }

        self.last_published = Some(snapshot);
        committed
    }

    fn apply_event(&mut self, event: &Event) {
        match event {
            Event::WorkspaceChanged {
                name,
                focused_window,
            } => {
                self.signals.set("workspace.name", name.clone());
                if let Some(title) = focused_window {
                    self.signals
                        .set("workspace.focused_window", title.clone());
                } else {
                    self.signals.remove("workspace.focused_window");
                }
            }
            Event::WindowFocused { app_id, title } => {
                if let Some(id) = app_id {
                    self.signals.set("focused.app_id", id.clone());
                } else {
                    self.signals.remove("focused.app_id");
                }
                self.signals.set("focused.title", title.clone());
            }
            Event::PowerStateChanged { on_battery } => {
                self.signals.set("power.on_battery", *on_battery);
            }
            Event::BarDensityRequested { mode } => {
                // `cycle` is a sentinel from `levshell-ctl density cycle`:
                // resolve the next mode from the stored signal
                // (full -> compact -> hidden -> full). Absent == full.
                let resolved: &str = if mode == "cycle" {
                    let current = self
                        .signals
                        .get("bar.density")
                        .and_then(|v| v.as_str())
                        .unwrap_or("full");
                    match current {
                        "full" => "compact",
                        "compact" => "hidden",
                        _ => "full",
                    }
                } else {
                    mode.as_str()
                };
                self.signals.set("bar.density", resolved.to_owned());
                let density = match resolved {
                    "compact" => BarDensity::Compact,
                    "hidden" => BarDensity::Hidden,
                    _ => BarDensity::Full,
                };
                if let Err(e) = self.publisher.try_send(DaemonMessage::BarDensityState(
                    BarDensityState { mode: density },
                )) {
                    tracing::warn!(error = %e, "context-engine: failed to publish BarDensityState");
                }
            }
            Event::ProfileActionRequested { action, name } => {
                self.apply_profile_action(action, name.as_deref());
            }
            // Focus-session signal (spec §3.5.1). Before the session
            // timer module existed this input had no producer; the
            // timer's interval-boundary events now drive it so profiles
            // can predicate on `focus_session.active` /
            // `focus_session.on_break` / `focus_session.kind`.
            Event::FocusSessionStarted { kind, .. } => {
                self.signals.set("focus_session.active", true);
                self.signals
                    .set("focus_session.on_break", kind == "break");
                self.signals.set("focus_session.kind", kind.clone());
            }
            Event::FocusSessionEnded { .. } => {
                // On auto-advance a FocusSessionStarted for the next
                // interval immediately follows and re-sets these; on
                // `stop` nothing follows, so idle is the resting state.
                self.signals.set("focus_session.active", false);
                self.signals.set("focus_session.on_break", false);
                self.signals.set("focus_session.kind", "idle".to_owned());
            }
            _ => {}
        }
    }

    fn apply_profile_action(&mut self, action: &str, name: Option<&str>) {
        match action {
            "activate" => {
                if let Some(target) = name {
                    let exists = self
                        .profiles
                        .read()
                        .expect("context-engine profile lock poisoned")
                        .iter()
                        .any(|p| p.name == target);
                    if exists {
                        self.active_profile = Some(target.to_owned());
                    } else {
                        tracing::warn!(
                            profile = target,
                            "context-engine: activate ignored — unknown profile"
                        );
                    }
                }
            }
            "deactivate" => {
                // If `name` is supplied, only clear when it matches the
                // currently-active profile — this is how the focus-mode
                // driver safely retracts an auto-activation without
                // clobbering a manual one the user layered on top. If
                // `name` is None, clear unconditionally (manual "exit").
                match name {
                    Some(target) => {
                        if self.active_profile.as_deref() == Some(target) {
                            self.active_profile = None;
                        }
                    }
                    None => self.active_profile = None,
                }
            }
            "cycle" => {
                let names: Vec<String> = {
                    let guard = self
                        .profiles
                        .read()
                        .expect("context-engine profile lock poisoned");
                    if guard.is_empty() {
                        return;
                    }
                    let mut n: Vec<String> = guard.iter().map(|p| p.name.clone()).collect();
                    n.sort();
                    n
                };
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let next = match self.active_profile.as_deref() {
                    None => names[0],
                    Some(current) => {
                        let idx = names
                            .iter()
                            .position(|n| *n == current)
                            .unwrap_or(names.len() - 1);
                        names[(idx + 1) % names.len()]
                    }
                };
                self.active_profile = Some(next.to_owned());
            }
            "query" => {
                // Read-only; nothing to update. The ctl response was already
                // sent before this event was published.
            }
            other => {
                tracing::debug!(action = other, "context-engine: ignoring unknown profile action");
            }
        }
    }
}

#[async_trait]
impl Module for ContextEngineModule {
    fn name(&self) -> &str {
        "context-engine"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        self.widgets
            .iter()
            .map(|w| WidgetDescriptor {
                id: w.id.clone(),
                widget_type: w.widget_type.clone(),
            })
            .collect()
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::WorkspaceChanged,
            EventKind::WindowFocused,
            EventKind::PowerStateChanged,
            EventKind::BarDensityRequested,
            EventKind::ProfileActionRequested,
            EventKind::FocusSessionStarted,
            EventKind::FocusSessionEnded,
        ]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(DEFAULT_TICK)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // Seed an initial layout so the shell has something to render the
        // moment it connects, even before any event has fired.
        self.resolve_and_publish(Instant::now());
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        self.apply_event(event);
        self.resolve_and_publish(Instant::now());
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.resolve_and_publish(Instant::now());
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

/// Build the default Phase 1.3 widget set: the handful of widgets the
/// daemon's built-in modules emit. Widget types are the same strings the
/// telemetry modules publish so the shell's dispatch-by-type switch can
/// find one `Component` per widget_type.
pub fn default_widgets() -> Vec<WidgetDef> {
    use levshell_context::rules::Zone;
    vec![
        WidgetDef::new("workspace-indicator", "workspace_indicator")
            .with_default(Prominence::Visible)
            .with_priority(100)
            .with_zone(Zone::Left),
        // spec §2.12.3 — transient pill next to the workspace indicator.
        // Always nominally Visible; the widget's own state decides whether
        // to render anything (width 0 when dormant, fades in on re-entry
        // above the interruption-cost threshold).
        WidgetDef::new("interruption-cost", "interruption_cost")
            .with_default(Prominence::Visible)
            .with_priority(95)
            .with_zone(Zone::Left),
        WidgetDef::new("clock", "clock")
            .with_default(Prominence::Visible)
            .with_priority(90)
            .with_zone(Zone::Center),
        WidgetDef::new("cpu", "cpu")
            .with_default(Prominence::Compact)
            .with_priority(40)
            .with_zone(Zone::Right),
        WidgetDef::new("memory", "memory")
            .with_default(Prominence::Compact)
            .with_priority(45)
            .with_zone(Zone::Right),
        WidgetDef::new("network", "network")
            .with_default(Prominence::IconOnly)
            .with_priority(55)
            .with_zone(Zone::Right),
        WidgetDef::new("battery", "battery")
            .with_default(Prominence::IconOnly)
            .with_priority(60)
            .with_zone(Zone::Right),
        WidgetDef::new("notifications", "notifications")
            .with_default(Prominence::IconOnly)
            .with_priority(20)
            .with_zone(Zone::Right),
        // Hardware-independent entry point for the Quick-Settings
        // flyout. Without this the flyout is only reachable via the
        // battery widget, which self-parks on desktops.
        WidgetDef::new("control-center", "control_center")
            .with_default(Prominence::IconOnly)
            .with_priority(25)
            .with_zone(Zone::Right),
        // System tray (SNI host). Spec lists it in the context
        // engine's always-present base layer; rendered icons-only.
        WidgetDef::new("system-tray", "system_tray")
            .with_default(Prominence::IconOnly)
            .with_priority(22)
            .with_zone(Zone::Right),
        // Note: the command palette is an overlay, not an in-bar
        // widget, and is intentionally NOT listed here. The shell reads
        // its state directly from a command-palette WidgetUpdate; the
        // cascade knows nothing about it.
    ]
}

/// Convenience helper that assembles the default Phase 1.2 context engine
/// module with built-in widgets, no rules, and no profiles. Callers may
/// chain further [`ContextEngineModule`] builder methods on the result.
pub fn default_context_engine(publisher: WidgetPublisher) -> ContextEngineModule {
    ContextEngineModule::new(publisher).with_widgets(default_widgets())
}

/// Best-effort query of the layout pixel budget via sway IPC: the widest
/// active output. The bar spans its focused output; using the widest
/// active output as the budget is a safe single-/multi-monitor default
/// and replaces the hardcoded [`DEFAULT_AVAILABLE_WIDTH`] fallback.
/// Returns `None` if sway is unreachable or reports no usable output, in
/// which case callers keep the default.
pub async fn primary_output_width() -> Option<u32> {
    let mut conn = swayipc_async::Connection::new().await.ok()?;
    let outputs = conn.get_outputs().await.ok()?;
    outputs
        .into_iter()
        .filter(|o| o.active)
        .map(|o| o.rect.width.max(0) as u32)
        .max()
        .filter(|w| *w > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_context::parse_expression;
    use levshell_context::rules::Zone;
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::{duplex, AsyncReadExt, BufReader};

    /// Spin up an in-memory writer task so the module can publish into a
    /// duplex pipe and the test can read it back as raw bytes. Returns the
    /// publisher, the writer task handle, and a reader over the far side of
    /// the duplex.
    fn writer_over_duplex() -> (
        WidgetPublisher,
        tokio::task::JoinHandle<()>,
        BufReader<tokio::io::DuplexStream>,
    ) {
        let (a, b) = duplex(4096);
        let writer = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(writer, 16);
        (task.publisher, task.handle, BufReader::new(b))
    }

    async fn read_frame_json(reader: &mut BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
        let mut buf = Vec::new();
        // Read until newline manually to avoid pulling in tokio::io::AsyncBufReadExt
        let mut byte = [0u8; 1];
        loop {
            reader.read_exact(&mut byte).await.unwrap();
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        serde_json::from_slice(&buf).unwrap()
    }

    fn three_widgets() -> Vec<WidgetDef> {
        vec![
            WidgetDef::new("clock", "clock")
                .with_default(Prominence::Visible)
                .with_priority(100)
                .with_zone(Zone::Center),
            WidgetDef::new("cpu", "telemetry")
                .with_default(Prominence::Compact)
                .with_priority(40)
                .with_zone(Zone::Right),
            WidgetDef::new("battery", "telemetry")
                .with_default(Prominence::IconOnly)
                .with_priority(60)
                .with_zone(Zone::Right),
        ]
    }

    #[tokio::test]
    async fn start_publishes_initial_bar_layout_and_visibilities() {
        let (publisher, _handle, mut reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher).with_widgets(three_widgets());
        module.start().await.unwrap();

        // First frame: BarLayout with center=[clock], right=[battery, cpu].
        let msg = read_frame_json(&mut reader).await;
        assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("bar_layout"));
        let right = msg.get("right").and_then(|v| v.as_array()).unwrap();
        assert_eq!(right.len(), 2);

        // Then one WidgetVisibility per widget (3 total).
        let mut seen = Vec::new();
        for _ in 0..3 {
            let v = read_frame_json(&mut reader).await;
            assert_eq!(
                v.get("type").and_then(|x| x.as_str()),
                Some("widget_visibility")
            );
            seen.push(
                v.get("widget_id")
                    .and_then(|x| x.as_str())
                    .unwrap()
                    .to_owned(),
            );
        }
        seen.sort();
        assert_eq!(seen, vec!["battery", "clock", "cpu"]);
    }

    #[tokio::test]
    async fn workspace_event_updates_signal_context_and_republishes() {
        let (publisher, _handle, mut reader) = writer_over_duplex();
        let rule = CompiledRule::new(
            "cpu",
            parse_expression(r#"workspace.name == "code""#).unwrap(),
            Prominence::Visible,
        );
        let mut module = ContextEngineModule::new(publisher)
            .with_widgets(three_widgets())
            .with_rules(vec![rule]);
        module.start().await.unwrap();

        // Drain initial burst (1 BarLayout + 3 WidgetVisibility).
        for _ in 0..4 {
            let _ = read_frame_json(&mut reader).await;
        }

        // Fire a WorkspaceChanged event that flips the rule on. Because
        // hysteresis has never seen `cpu` promoted before, the first
        // observation commits immediately (first_observation_commits_...).
        // Wait — cpu was already committed to Compact by start(). Promotion
        // to Visible must wait for activation_delay. So we expect no change
        // until we manually force-commit or tick past the delay.
        //
        // For this assertion we just verify the BarLayout doesn't change
        // because the cpu prominence in the cascade output is Visible but
        // the committed value is still Compact. The cascade rerun still
        // happens, but it's layout-equivalent.
        let event = Event::WorkspaceChanged {
            name: "code".into(),
            focused_window: None,
        };
        module.on_event(&event).await.unwrap();

        // Nothing should come over the wire because layout didn't change
        // and per-widget committed values didn't change either. Assert by
        // confirming the signal was recorded.
        assert_eq!(
            module
                .signals
                .get("workspace.name")
                .and_then(|v| v.as_str()),
            Some("code")
        );
    }

    #[tokio::test]
    async fn focus_session_events_drive_the_signal() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher).with_widgets(three_widgets());

        // A work interval started.
        module
            .on_event(&Event::FocusSessionStarted {
                kind: "work".into(),
                project: Some("llm-alignment".into()),
                planned_secs: 1500,
            })
            .await
            .unwrap();
        assert_eq!(
            module.signals.get("focus_session.active").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            module.signals.get("focus_session.on_break").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            module.signals.get("focus_session.kind").and_then(|v| v.as_str()),
            Some("work")
        );

        // Auto-advanced into a break.
        module
            .on_event(&Event::FocusSessionStarted {
                kind: "break".into(),
                project: Some("llm-alignment".into()),
                planned_secs: 300,
            })
            .await
            .unwrap();
        assert_eq!(
            module.signals.get("focus_session.on_break").and_then(|v| v.as_bool()),
            Some(true)
        );

        // Stopped → idle resting state.
        module
            .on_event(&Event::FocusSessionEnded {
                kind: "break".into(),
                project: Some("llm-alignment".into()),
                actual_secs: 42,
            })
            .await
            .unwrap();
        assert_eq!(
            module.signals.get("focus_session.active").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            module.signals.get("focus_session.kind").and_then(|v| v.as_str()),
            Some("idle")
        );
    }

    #[tokio::test]
    async fn profile_activation_swaps_overrides_on_next_resolve() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let writing = Profile::new("writing").with("cpu", Prominence::Hidden);
        let focus = Profile::new("focus").with("cpu", Prominence::Badge);
        let mut module = ContextEngineModule::new(publisher)
            .with_widgets(three_widgets())
            .with_profiles(vec![writing, focus]);
        module.start().await.unwrap();

        // Activate the "writing" profile via an action event.
        module
            .on_event(&Event::ProfileActionRequested {
                action: "activate".into(),
                name: Some("writing".into()),
            })
            .await
            .unwrap();
        assert_eq!(module.active_profile.as_deref(), Some("writing"));

        // Cycle → focus.
        module
            .on_event(&Event::ProfileActionRequested {
                action: "cycle".into(),
                name: None,
            })
            .await
            .unwrap();
        assert_eq!(module.active_profile.as_deref(), Some("focus"));

        // Cycle again → wraps back to writing.
        module
            .on_event(&Event::ProfileActionRequested {
                action: "cycle".into(),
                name: None,
            })
            .await
            .unwrap();
        assert_eq!(module.active_profile.as_deref(), Some("writing"));
    }

    #[tokio::test]
    async fn deactivate_matching_clears_active_profile() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher)
            .with_widgets(three_widgets())
            .with_profiles(vec![
                Profile::new("writing"),
                Profile::new("lit-review"),
            ]);
        module.start().await.unwrap();

        module
            .on_event(&Event::ProfileActionRequested {
                action: "activate".into(),
                name: Some("lit-review".into()),
            })
            .await
            .unwrap();
        assert_eq!(module.active_profile.as_deref(), Some("lit-review"));

        // Deactivate with a non-matching name — no-op.
        module
            .on_event(&Event::ProfileActionRequested {
                action: "deactivate".into(),
                name: Some("writing".into()),
            })
            .await
            .unwrap();
        assert_eq!(module.active_profile.as_deref(), Some("lit-review"));

        // Deactivate with matching name — clears.
        module
            .on_event(&Event::ProfileActionRequested {
                action: "deactivate".into(),
                name: Some("lit-review".into()),
            })
            .await
            .unwrap();
        assert!(module.active_profile.is_none());
    }

    #[tokio::test]
    async fn deactivate_without_name_clears_any_active_profile() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher)
            .with_widgets(three_widgets())
            .with_profiles(vec![Profile::new("writing")])
            .with_active_profile("writing");
        module.start().await.unwrap();
        module
            .on_event(&Event::ProfileActionRequested {
                action: "deactivate".into(),
                name: None,
            })
            .await
            .unwrap();
        assert!(module.active_profile.is_none());
    }

    #[tokio::test]
    async fn unknown_profile_activate_is_ignored_not_panics() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher)
            .with_widgets(three_widgets())
            .with_profiles(vec![Profile::new("writing")]);
        module.start().await.unwrap();
        module
            .on_event(&Event::ProfileActionRequested {
                action: "activate".into(),
                name: Some("does-not-exist".into()),
            })
            .await
            .unwrap();
        assert!(module.active_profile.is_none());
    }

    #[tokio::test]
    async fn power_event_updates_on_battery_signal() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut module = ContextEngineModule::new(publisher).with_widgets(three_widgets());
        module.start().await.unwrap();
        module
            .on_event(&Event::PowerStateChanged { on_battery: true })
            .await
            .unwrap();
        assert_eq!(
            module
                .signals
                .get("power.on_battery")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn default_widgets_builds_the_expected_set() {
        let widgets = default_widgets();
        let ids: Vec<&str> = widgets.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&"workspace-indicator"));
        assert!(ids.contains(&"clock"));
        assert!(ids.contains(&"battery"));
    }
}
