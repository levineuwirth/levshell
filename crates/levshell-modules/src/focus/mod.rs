//! Focus-mode driver — auto-activates context profiles on sustained
//! signal matches (spec §2.12.4 literature review mode, §2.12.5 writing
//! mode).
//!
//! ## What it does
//!
//! For every profile with an `auto_trigger`, this module evaluates the
//! predicate against a mirrored [`SignalContext`] on every bus event and
//! tick. Sustained-true for `dwell` seconds publishes
//! [`Event::ProfileActionRequested`] with `"activate"`; sustained-false
//! for `exit_dwell` seconds publishes `"deactivate"` with the profile
//! name. The context engine consumes those events via its existing
//! profile-action handler.
//!
//! ## Why this is its own module
//!
//! The context engine resolves *what the layout looks like* for the
//! currently-active profile. Deciding *which profile should be active*
//! based on time-windowed focus observations is a separate concern with
//! its own state machine (see [`tracker::FocusModeTracker`]) — and we
//! want it to run even when the context engine has nothing to do.
//!
//! ## Extensibility
//!
//! Triggers are arbitrary predicates in the existing expression DSL over
//! any named signal (`focused.app_id`, `focused.title`,
//! `workspace.name`, `workspace.tags`, `battery.percent`, `power.on_battery`,
//! …). Zotero is just one `or`-branch in the shipped `lit-review.toml`
//! example; any app, workspace name, title substring, or future signal
//! can drive activation without touching this module.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use levshell_context::{evaluate, Profile, SignalContext};
use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult, WidgetDescriptor};

pub mod tracker;

pub use tracker::{FocusModeTracker, TriggerAction, TriggerInput, TriggerPhase};

pub const MODULE_NAME: &str = "focus-mode";

/// Default poll interval. Short enough that dwell transitions commit
/// promptly, long enough that we're not churning the bus during an
/// idle session.
const DEFAULT_TICK: Duration = Duration::from_millis(1000);

pub struct FocusModeModule {
    bus: EventBus,
    profiles: Arc<RwLock<Vec<Profile>>>,
    signals: SignalContext,
    tracker: FocusModeTracker,
    tick_interval: Duration,
}

impl FocusModeModule {
    pub fn new(bus: EventBus, profiles: Arc<RwLock<Vec<Profile>>>) -> Self {
        Self {
            bus,
            profiles,
            signals: SignalContext::new(),
            tracker: FocusModeTracker::new(),
            tick_interval: DEFAULT_TICK,
        }
    }

    pub fn with_tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Mirror the signals the context engine does so our predicates see
    /// the same view. Kept in sync here because focus-mode needs to
    /// evaluate triggers independently of whether the context engine has
    /// ticked yet (it runs on a separate interval).
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
            _ => {}
        }
    }

    /// Build a snapshot of current trigger inputs by reading profiles
    /// out of the shared lock and evaluating each predicate against the
    /// current signal context. The returned owned strings decouple the
    /// call from the profile lock for the subsequent `tracker.tick`.
    fn snapshot_inputs(&self) -> Vec<OwnedInput> {
        let guard = match self.profiles.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!("focus-mode: profile lock poisoned; no triggers evaluated");
                return Vec::new();
            }
        };
        guard
            .iter()
            .filter_map(|p| {
                let t = p.auto_trigger.as_ref()?;
                let predicate = eval_predicate(&t.when, &self.signals);
                Some(OwnedInput {
                    profile: p.name.clone(),
                    predicate,
                    dwell: t.dwell,
                    exit_dwell: t.exit_dwell,
                })
            })
            .collect()
    }

    /// Pure-ish core: evaluate every profile's trigger at `now`, advance
    /// the tracker, publish any actions to the bus. Exposed so tests can
    /// drive arbitrary `Instant` timelines without a paused tokio clock.
    pub fn evaluate_and_publish(&mut self, now: Instant) {
        let inputs = self.snapshot_inputs();
        let borrowed: Vec<TriggerInput<'_>> = inputs
            .iter()
            .map(|i| TriggerInput {
                profile: &i.profile,
                predicate: i.predicate,
                dwell: i.dwell,
                exit_dwell: i.exit_dwell,
            })
            .collect();
        let actions = self.tracker.tick(&borrowed, now);
        for action in actions {
            let event = match action {
                TriggerAction::Activate(name) => Event::ProfileActionRequested {
                    action: "activate".into(),
                    name: Some(name),
                },
                TriggerAction::Deactivate(name) => Event::ProfileActionRequested {
                    action: "deactivate".into(),
                    name: Some(name),
                },
            };
            self.bus.publish(event);
        }
    }
}

/// Evaluate a predicate, swallowing type-mismatch errors as `false` (same
/// policy as the context engine's `CompiledRule::evaluate`). A broken
/// trigger logs a warning and stays dormant rather than crashing the
/// module.
fn eval_predicate(expr: &levshell_context::Expression, ctx: &SignalContext) -> bool {
    match evaluate(expr, ctx) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "focus-mode: trigger expression eval error; treating as false"
            );
            false
        }
    }
}

/// Owned trigger input so we can drop the profile read-lock before
/// calling the tracker.
struct OwnedInput {
    profile: String,
    predicate: bool,
    dwell: Duration,
    exit_dwell: Duration,
}

#[async_trait]
impl Module for FocusModeModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        Vec::new()
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::WindowFocused,
            EventKind::WorkspaceChanged,
            EventKind::PowerStateChanged,
        ]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(self.tick_interval)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // No initial publish — the tracker starts Inactive for every
        // profile and will only emit after at least `dwell` seconds of
        // sustained true.
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        self.apply_event(event);
        self.evaluate_and_publish(Instant::now());
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.evaluate_and_publish(Instant::now());
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use levshell_context::{parse_expression, AutoTrigger};
    use levshell_core::EventKind;

    fn writing_profile(dwell_secs: u64, exit_secs: u64) -> Profile {
        Profile::new("writing").with_auto_trigger(AutoTrigger {
            when: parse_expression(r#"focused.app_id == "neovide""#).unwrap(),
            dwell: Duration::from_secs(dwell_secs),
            exit_dwell: Duration::from_secs(exit_secs),
        })
    }

    #[tokio::test]
    async fn activates_after_sustained_focus() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("test", vec![EventKind::ProfileActionRequested], 16);

        let profiles = Arc::new(RwLock::new(vec![writing_profile(2, 5)]));
        let mut module = FocusModeModule::new(bus, profiles);

        let t0 = Instant::now();
        // Apply a WindowFocused that flips the predicate true.
        module.apply_event(&Event::WindowFocused {
            app_id: Some("neovide".into()),
            title: "draft.md".into(),
        });
        module.evaluate_and_publish(t0);
        assert!(rx.try_recv().is_err(), "no activate before dwell elapses");

        // Advance past dwell — one more tick should emit activate.
        module.evaluate_and_publish(t0 + Duration::from_secs(3));
        let event = rx.recv().await.expect("activate event");
        match event {
            Event::ProfileActionRequested { action, name } => {
                assert_eq!(action, "activate");
                assert_eq!(name.as_deref(), Some("writing"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deactivates_after_sustained_non_match() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("test", vec![EventKind::ProfileActionRequested], 16);

        let profiles = Arc::new(RwLock::new(vec![writing_profile(2, 3)]));
        let mut module = FocusModeModule::new(bus, profiles);

        let t0 = Instant::now();
        // Activate first.
        module.apply_event(&Event::WindowFocused {
            app_id: Some("neovide".into()),
            title: "x".into(),
        });
        module.evaluate_and_publish(t0);
        module.evaluate_and_publish(t0 + Duration::from_secs(3));
        let _ = rx.recv().await.expect("activate");

        // Switch to non-matching app.
        module.apply_event(&Event::WindowFocused {
            app_id: Some("firefox".into()),
            title: "news".into(),
        });
        module.evaluate_and_publish(t0 + Duration::from_secs(3));
        assert!(rx.try_recv().is_err(), "no deactivate before exit_dwell");

        // Past exit_dwell (3s) — should emit deactivate.
        module.evaluate_and_publish(t0 + Duration::from_secs(7));
        let event = rx.recv().await.expect("deactivate event");
        match event {
            Event::ProfileActionRequested { action, name } => {
                assert_eq!(action, "deactivate");
                assert_eq!(name.as_deref(), Some("writing"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn flicker_during_dwell_is_silent() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("test", vec![EventKind::ProfileActionRequested], 16);

        let profiles = Arc::new(RwLock::new(vec![writing_profile(5, 5)]));
        let mut module = FocusModeModule::new(bus, profiles);

        let t0 = Instant::now();
        module.apply_event(&Event::WindowFocused {
            app_id: Some("neovide".into()),
            title: "x".into(),
        });
        module.evaluate_and_publish(t0);

        // Alt-tab away before dwell elapses — tracker must drop back to
        // Inactive without ever emitting an activate.
        module.apply_event(&Event::WindowFocused {
            app_id: Some("firefox".into()),
            title: "y".into(),
        });
        module.evaluate_and_publish(t0 + Duration::from_secs(2));
        module.evaluate_and_publish(t0 + Duration::from_secs(10));
        assert!(rx.try_recv().is_err(), "no activate after cancelled dwell");
    }

    #[tokio::test]
    async fn malformed_trigger_does_not_crash_module() {
        // A trigger that references a non-existent signal just evaluates
        // false on every tick — the profile never activates. Verified by
        // confirming no events and no panic.
        let bus = EventBus::new();
        let mut rx = bus.subscribe("test", vec![EventKind::ProfileActionRequested], 16);

        let profile = Profile::new("broken").with_auto_trigger(AutoTrigger {
            when: parse_expression("not_a_signal == 42").unwrap(),
            dwell: Duration::from_secs(1),
            exit_dwell: Duration::from_secs(1),
        });
        let profiles = Arc::new(RwLock::new(vec![profile]));
        let mut module = FocusModeModule::new(bus, profiles);

        let t0 = Instant::now();
        for i in 0..5 {
            module.evaluate_and_publish(t0 + Duration::from_secs(i * 2));
        }
        assert!(rx.try_recv().is_err());
    }
}
