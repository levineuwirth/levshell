//! Pomodoro / focus-session timer (spec §2.2.1, §3.6).
//!
//! A classic work/break interval timer. It is also the **producer for
//! the context engine's `focus_session` input signal** (spec §3.5.1):
//! every interval boundary emits `Event::FocusSessionStarted` /
//! `Event::FocusSessionEnded` on the bus, which the context-engine
//! module folds into a signal so profiles can react to focus state
//! ("hide notifications while in a work interval"). Before this module
//! existed that signal had no producer — closing that gap is the point
//! of Milestone 2.
//!
//! ## Surfaces
//!
//! - **Control:** `ctl timer {start|pause|resume|stop|skip}` →
//!   `CtlRequest::Timer` → bus `Event::SessionTimerCommand` → here.
//! - **Render:** a `session-timer` `WidgetUpdate` published every tick
//!   while running (the bar pill, built in M2.8).
//! - **Logging:** focus seconds are accumulated per workspace in memory
//!   and logged on each completed work interval (spec §2.2.1
//!   "auto-logging of sessions per workspace"). Durable journaling is a
//!   later feature; this is the minimal honest version.
//!
//! State machine (auto-advancing Pomodoro):
//!
//! ```text
//!   Idle --start--> Work --(elapsed≥work)--> Break --(≥break)--> Work ...
//!                     ^                                            |
//!                     +----- every Nth work interval: LongBreak ---+
//! ```
//! `pause`/`resume` freeze the elapsed counter; `stop` returns to Idle;
//! `skip` ends the current interval immediately and advances.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde::Deserialize;

use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};

pub const SESSION_TIMER_WIDGET_ID: &str = "session-timer";
pub const SESSION_TIMER_WIDGET_TYPE: &str = "session_timer";
const MODULE_NAME: &str = "session-timer";

/// One real second per tick — the pill counts down visibly.
const TICK: StdDuration = StdDuration::from_secs(1);

fn d_work() -> u64 {
    25
}
fn d_break() -> u64 {
    5
}
fn d_long_break() -> u64 {
    15
}
fn d_until_long() -> u32 {
    4
}

/// `~/.config/levshell/modules/session_timer.toml` (all fields
/// optional; the classic 25/5/15-after-4 Pomodoro is the default).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SessionTimerConfig {
    pub work_minutes: u64,
    pub break_minutes: u64,
    pub long_break_minutes: u64,
    pub intervals_until_long_break: u32,
}

impl Default for SessionTimerConfig {
    fn default() -> Self {
        Self {
            work_minutes: d_work(),
            break_minutes: d_break(),
            long_break_minutes: d_long_break(),
            intervals_until_long_break: d_until_long(),
        }
    }
}

impl SessionTimerConfig {
    /// Load `session_timer.toml` from a config dir, falling back to
    /// defaults if it's absent or malformed (a broken timer config must
    /// not stop the daemon from starting).
    pub fn load_from_dir(dir: &std::path::Path) -> Self {
        let path = dir.join("session_timer.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e,
                        "session_timer.toml malformed; using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Work,
    Break,
}

impl Phase {
    fn wire(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Work => "work",
            Phase::Break => "break",
        }
    }
}

pub struct SessionTimerModule {
    bus: EventBus,
    publisher: WidgetPublisher,
    cfg: SessionTimerConfig,

    phase: Phase,
    paused: bool,
    elapsed_secs: u64,
    planned_secs: u64,
    /// Completed work intervals since the last Idle — drives the
    /// long-break cadence.
    work_done: u32,
    current_project: Option<String>,
    /// Focus seconds per workspace, accumulated in memory. Logged on
    /// each completed work interval; reset on daemon restart.
    per_ws_secs: HashMap<String, u64>,
}

impl SessionTimerModule {
    pub fn new(bus: EventBus, publisher: WidgetPublisher, cfg: SessionTimerConfig) -> Self {
        Self {
            bus,
            publisher,
            cfg,
            phase: Phase::Idle,
            paused: false,
            elapsed_secs: 0,
            planned_secs: 0,
            work_done: 0,
            current_project: None,
            per_ws_secs: HashMap::new(),
        }
    }

    fn work_secs(&self) -> u64 {
        self.cfg.work_minutes.max(1) * 60
    }
    fn break_secs(&self) -> u64 {
        // Long break replaces the short one every Nth completed work
        // interval (classic Pomodoro).
        let n = self.cfg.intervals_until_long_break.max(1);
        if self.work_done > 0 && self.work_done % n == 0 {
            self.cfg.long_break_minutes.max(1) * 60
        } else {
            self.cfg.break_minutes.max(1) * 60
        }
    }

    /// Begin `phase`, publish `FocusSessionStarted`, push the widget.
    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.paused = false;
        self.elapsed_secs = 0;
        self.planned_secs = match phase {
            Phase::Idle => 0,
            Phase::Work => self.work_secs(),
            Phase::Break => self.break_secs(),
        };
        if phase != Phase::Idle {
            self.bus.publish(Event::FocusSessionStarted {
                kind: phase.wire().to_owned(),
                project: self.current_project.clone(),
                planned_secs: self.planned_secs,
            });
        }
        self.publish_widget();
    }

    /// End the current interval (`actual` = seconds spent), emit
    /// `FocusSessionEnded`, and book work time against the workspace.
    fn end_current(&mut self, actual: u64) {
        if self.phase == Phase::Idle {
            return;
        }
        if self.phase == Phase::Work {
            self.work_done += 1;
            if let Some(ws) = self.current_project.clone() {
                *self.per_ws_secs.entry(ws.clone()).or_insert(0) += actual;
                tracing::info!(
                    workspace = %ws,
                    interval_secs = actual,
                    total_secs = self.per_ws_secs[&ws],
                    "session-timer: logged work interval"
                );
            }
        }
        self.bus.publish(Event::FocusSessionEnded {
            kind: self.phase.wire().to_owned(),
            project: self.current_project.clone(),
            actual_secs: actual,
        });
    }

    /// Move from a finished interval to the next one (Work→Break,
    /// Break→Work). Used by both natural completion and `skip`.
    fn advance(&mut self) {
        let actual = self.elapsed_secs;
        let next = match self.phase {
            Phase::Work => Phase::Break,
            Phase::Break => Phase::Work,
            Phase::Idle => return,
        };
        self.end_current(actual);
        self.enter(next);
    }

    fn handle_command(&mut self, action: &str) {
        match action {
            "start" => match self.phase {
                Phase::Idle => {
                    self.work_done = 0;
                    self.enter(Phase::Work);
                }
                _ if self.paused => {
                    self.paused = false;
                    self.publish_widget();
                }
                _ => {} // already running
            },
            "pause" => {
                if self.phase != Phase::Idle && !self.paused {
                    self.paused = true;
                    self.publish_widget();
                }
            }
            "resume" => {
                if self.paused {
                    self.paused = false;
                    self.publish_widget();
                }
            }
            "stop" => {
                if self.phase != Phase::Idle {
                    self.end_current(self.elapsed_secs);
                    self.work_done = 0;
                    self.enter(Phase::Idle);
                }
            }
            "skip" => {
                if self.phase != Phase::Idle {
                    self.advance();
                }
            }
            other => tracing::debug!(action = other, "session-timer: unknown command"),
        }
    }

    fn publish_widget(&self) {
        let update = WidgetUpdate {
            widget_id: SESSION_TIMER_WIDGET_ID.into(),
            widget_type: SESSION_TIMER_WIDGET_TYPE.into(),
            state: serde_json::json!({
                "phase": self.phase.wire(),
                "paused": self.paused,
                "elapsed_secs": self.elapsed_secs,
                "planned_secs": self.planned_secs,
                "work_intervals": self.work_done,
            }),
            status: WidgetStatus::Normal,
            escalation: Default::default(),
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "session-timer: widget publish drop");
        }
    }

    /// One real-time second of progress. Returns nothing; transitions
    /// and publishing happen as side effects.
    fn advance_one_second(&mut self) {
        if self.phase == Phase::Idle || self.paused {
            return;
        }
        self.elapsed_secs += 1;
        if self.elapsed_secs >= self.planned_secs {
            self.advance();
        } else {
            self.publish_widget();
        }
    }
}

#[async_trait]
impl Module for SessionTimerModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: SESSION_TIMER_WIDGET_ID.into(),
            widget_type: SESSION_TIMER_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(TICK)
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::SessionTimerCommand,
            EventKind::WorkspaceChanged,
            EventKind::WidgetActionReceived,
        ]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // Publish the idle state so the pill can render "not running"
        // immediately rather than waiting for the first command.
        self.publish_widget();
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::SessionTimerCommand { action } => self.handle_command(action),
            Event::WorkspaceChanged { name, .. } => {
                self.current_project = Some(name.clone());
            }
            // The bar pill sends a single `toggle` (M1.1 passthrough);
            // resolve it to the right command from the current phase so
            // one click cycles idle→running→paused→running.
            Event::WidgetActionReceived {
                widget_id, action, ..
            } if widget_id == SESSION_TIMER_WIDGET_ID && action == "toggle" => {
                let resolved = match (self.phase, self.paused) {
                    (Phase::Idle, _) => "start",
                    (_, true) => "resume",
                    (_, false) => "pause",
                };
                self.handle_command(resolved);
            }
            _ => {}
        }
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.advance_one_second();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_core::EventKind;
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::duplex;

    fn module(cfg: SessionTimerConfig) -> (SessionTimerModule, EventBus) {
        let bus = EventBus::new();
        let (a, _b) = duplex(8192);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 64);
        (
            SessionTimerModule::new(bus.clone(), task.publisher, cfg),
            bus,
        )
    }

    fn fast_cfg() -> SessionTimerConfig {
        // 1-minute everything so a couple of ticks isn't enough but the
        // math stays trivial; we drive elapsed directly in unit tests.
        SessionTimerConfig {
            work_minutes: 1,
            break_minutes: 1,
            long_break_minutes: 2,
            intervals_until_long_break: 2,
        }
    }

    #[test]
    fn default_config_is_classic_pomodoro() {
        let c = SessionTimerConfig::default();
        assert_eq!(c.work_minutes, 25);
        assert_eq!(c.break_minutes, 5);
        assert_eq!(c.long_break_minutes, 15);
        assert_eq!(c.intervals_until_long_break, 4);
    }

    #[tokio::test]
    async fn start_enters_work_and_emits_focus_started() {
        let (mut m, bus) = module(fast_cfg());
        let mut rx = bus.subscribe("t", [EventKind::FocusSessionStarted], 8);
        m.handle_command("start");
        assert_eq!(m.phase, Phase::Work);
        match rx.try_recv().expect("FocusSessionStarted") {
            Event::FocusSessionStarted { kind, planned_secs, .. } => {
                assert_eq!(kind, "work");
                assert_eq!(planned_secs, 60);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn work_completion_advances_to_break_with_paired_events() {
        let (mut m, bus) = module(fast_cfg());
        let mut rx = bus.subscribe(
            "t",
            [EventKind::FocusSessionStarted, EventKind::FocusSessionEnded],
            16,
        );
        m.handle_command("start"); // -> Work (drains a Started)
        let _ = rx.try_recv();
        // Drive to the end of the work interval.
        for _ in 0..60 {
            m.advance_one_second();
        }
        assert_eq!(m.phase, Phase::Break);
        assert_eq!(m.work_done, 1);
        let ended = rx.try_recv().expect("ended");
        let started = rx.try_recv().expect("started");
        assert!(matches!(ended, Event::FocusSessionEnded { ref kind, .. } if kind == "work"));
        assert!(matches!(started, Event::FocusSessionStarted { ref kind, .. } if kind == "break"));
    }

    #[tokio::test]
    async fn pause_freezes_elapsed() {
        let (mut m, _bus) = module(fast_cfg());
        m.handle_command("start");
        m.advance_one_second();
        m.advance_one_second();
        m.handle_command("pause");
        let frozen = m.elapsed_secs;
        m.advance_one_second();
        m.advance_one_second();
        assert_eq!(m.elapsed_secs, frozen, "paused timer must not advance");
        m.handle_command("resume");
        m.advance_one_second();
        assert_eq!(m.elapsed_secs, frozen + 1);
    }

    #[tokio::test]
    async fn stop_returns_to_idle_and_emits_ended() {
        let (mut m, bus) = module(fast_cfg());
        let mut rx = bus.subscribe("t", [EventKind::FocusSessionEnded], 8);
        m.handle_command("start");
        m.advance_one_second();
        m.handle_command("stop");
        assert_eq!(m.phase, Phase::Idle);
        assert!(matches!(
            rx.try_recv().expect("ended"),
            Event::FocusSessionEnded { .. }
        ));
    }

    #[tokio::test]
    async fn widget_toggle_cycles_idle_running_paused() {
        let (mut m, _bus) = module(fast_cfg());
        let toggle = || Event::WidgetActionReceived {
            widget_id: SESSION_TIMER_WIDGET_ID.into(),
            action: "toggle".into(),
            data: "{}".into(),
        };
        // idle -> start
        m.on_event(&toggle()).await.unwrap();
        assert_eq!(m.phase, Phase::Work);
        assert!(!m.paused);
        // running -> pause
        m.on_event(&toggle()).await.unwrap();
        assert!(m.paused);
        // paused -> resume
        m.on_event(&toggle()).await.unwrap();
        assert!(!m.paused);
        assert_eq!(m.phase, Phase::Work);

        // A toggle for a different widget is ignored.
        m.on_event(&Event::WidgetActionReceived {
            widget_id: "something-else".into(),
            action: "toggle".into(),
            data: "{}".into(),
        })
        .await
        .unwrap();
        assert!(!m.paused);
    }

    #[tokio::test]
    async fn long_break_after_n_work_intervals() {
        let (mut m, _bus) = module(fast_cfg()); // intervals_until_long_break = 2
        m.handle_command("start");
        // Interval 1: work then short break.
        m.handle_command("skip"); // end work #1 -> break
        assert_eq!(m.work_done, 1);
        assert_eq!(m.planned_secs, 60, "1st break is short");
        m.handle_command("skip"); // end break -> work #2
        m.handle_command("skip"); // end work #2 -> long break (work_done=2, 2%2==0)
        assert_eq!(m.work_done, 2);
        assert_eq!(m.planned_secs, 120, "2nd break is the long break");
    }
}
