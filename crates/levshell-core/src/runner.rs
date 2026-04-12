//! [`ModuleRunner`] — owns registered modules, drives their event/tick loops,
//! and tracks their health state per spec §5.2.2.
//!
//! For each registered module the runner:
//!
//!   1. Subscribes to the bus on the module's behalf for the event kinds
//!      returned by [`Module::subscribed_events`].
//!   2. Calls [`Module::start`]. If `start` returns
//!      [`ModuleError::Unavailable`] the module is parked in
//!      [`HealthState::Unavailable`] and no per-module task is spawned.
//!   3. Spawns one tokio task that selects between the event receiver, the
//!      module's tick interval, and a shutdown signal. Each `on_event` and
//!      `tick` call is wrapped in a timeout; the result is folded into the
//!      module's [`HealthState`].
//!   4. Returns a [`ModuleHandle`] holding a clone of the health-state arc
//!      so callers can inspect health without touching the module itself.
//!
//! [`Module::subscribed_events`]: crate::Module::subscribed_events
//! [`Module::start`]: crate::Module::start

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::bus::{Event, EventBus};
use crate::module::{Module, ModuleError, ModuleResult};

const HARD_EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const HARD_TICK_TIMEOUT_FALLBACK: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// HealthState
// ---------------------------------------------------------------------------

/// Per-module health, mirroring the four widget states from spec §5.2.1.
///
/// Transitions are managed entirely by the runner; modules never set their
/// own health. The runner reads the result of every `start` / `tick` /
/// `on_event` call and folds it into the state per spec §5.2.2:
///
/// * a successful call → `Normal`
/// * an explicit `ModuleError::Failed` → `Error`
/// * a tick that exceeds `2 × tick_interval` → `Stale`
/// * a `ModuleError::Unavailable` from `start` → `Unavailable` (terminal)
#[derive(Debug, Clone)]
pub enum HealthState {
    Normal,
    Stale { since: Instant, reason: String },
    Error { since: Instant, message: String },
    Unavailable { reason: String },
}

impl HealthState {
    /// String label matching the QML-side `status` field
    /// (`"normal"` / `"stale"` / `"error"` / `"unavailable"`) so that the
    /// IPC layer can serialize it without touching this enum.
    pub fn label(&self) -> &'static str {
        match self {
            HealthState::Normal => "normal",
            HealthState::Stale { .. } => "stale",
            HealthState::Error { .. } => "error",
            HealthState::Unavailable { .. } => "unavailable",
        }
    }
}

// ---------------------------------------------------------------------------
// ModuleHandle
// ---------------------------------------------------------------------------

/// Owner-side handle to a registered module. Holds the health-state arc
/// (cloneable, readable from outside the runner) and the join handle for
/// the per-module task. The shutdown channel is consumed on `shutdown`.
pub struct ModuleHandle {
    pub name: String,
    health: Arc<RwLock<HealthState>>,
    task: Option<JoinHandle<()>>,
    stop_tx: Option<oneshot::Sender<()>>,
}

impl ModuleHandle {
    pub fn health(&self) -> HealthState {
        self.health
            .read()
            .expect("module health lock poisoned")
            .clone()
    }

    pub fn health_handle(&self) -> Arc<RwLock<HealthState>> {
        self.health.clone()
    }
}

// ---------------------------------------------------------------------------
// ModuleRunner
// ---------------------------------------------------------------------------

pub struct ModuleRunner {
    bus: EventBus,
    handles: Vec<ModuleHandle>,
}

impl ModuleRunner {
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus,
            handles: Vec::new(),
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn handles(&self) -> &[ModuleHandle] {
        &self.handles
    }

    /// Register and start a module. Returns the index of the new
    /// [`ModuleHandle`] in [`Self::handles`]. If the module returns
    /// `ModuleError::Unavailable` from `start`, the handle is added in the
    /// `Unavailable` state and no background task is spawned.
    pub async fn register(&mut self, mut module: Box<dyn Module>) -> usize {
        let name = module.name().to_string();
        let kinds = module.subscribed_events();
        let tick_interval = module.tick_interval();
        let capacity = module.channel_capacity();

        let health = Arc::new(RwLock::new(HealthState::Normal));

        // Run start() and inspect the result before subscribing or spawning.
        match module.start().await {
            Ok(()) => {
                tracing::info!(module = %name, "module started");
            }
            Err(ModuleError::Unavailable(reason)) => {
                tracing::info!(module = %name, %reason, "module marked unavailable at start");
                *health.write().expect("module health lock poisoned") =
                    HealthState::Unavailable {
                        reason: reason.clone(),
                    };
                self.handles.push(ModuleHandle {
                    name,
                    health,
                    task: None,
                    stop_tx: None,
                });
                return self.handles.len() - 1;
            }
            Err(ModuleError::Failed(message)) => {
                tracing::warn!(module = %name, %message, "module start failed");
                *health.write().expect("module health lock poisoned") = HealthState::Error {
                    since: Instant::now(),
                    message,
                };
                // Continue: the loop may recover on a later tick.
            }
        }

        // Subscribe on the bus only if the module asked for any events.
        let event_rx = if kinds.is_empty() {
            None
        } else {
            Some(self.bus.subscribe(name.clone(), kinds, capacity))
        };

        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(module_loop(
            name.clone(),
            module,
            event_rx,
            stop_rx,
            tick_interval,
            health.clone(),
        ));

        self.handles.push(ModuleHandle {
            name,
            health,
            task: Some(task),
            stop_tx: Some(stop_tx),
        });
        self.handles.len() - 1
    }

    /// Find a handle by name. Returns `None` if no module with that name
    /// is registered.
    pub fn find(&self, name: &str) -> Option<&ModuleHandle> {
        self.handles.iter().find(|h| h.name == name)
    }

    /// Signal every running module to stop and wait for its task to drain.
    /// Modules in the `Unavailable` state have no task to join — they are
    /// silently dropped.
    pub async fn shutdown(mut self) {
        // First, send the stop signal to every module. Doing this in two
        // passes lets the modules unwind in parallel.
        for handle in &mut self.handles {
            if let Some(stop_tx) = handle.stop_tx.take() {
                let _ = stop_tx.send(());
            }
        }
        for handle in &mut self.handles {
            if let Some(task) = handle.task.take() {
                if let Err(e) = task.await {
                    tracing::warn!(module = %handle.name, error = %e, "module task join error");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-module loop
// ---------------------------------------------------------------------------

async fn module_loop(
    name: String,
    mut module: Box<dyn Module>,
    event_rx: Option<mpsc::Receiver<Event>>,
    mut stop_rx: oneshot::Receiver<()>,
    tick_interval: Option<Duration>,
    health: Arc<RwLock<HealthState>>,
) {
    // Build an interval timer if the module wants ticks. `MissedTickBehavior::Skip`
    // means a slow tick won't accumulate "missed" calls — we just resume on
    // the next tick boundary.
    let mut tick_timer = tick_interval.map(|d| {
        let mut t = interval(d);
        t.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Consume the immediate first tick so the loop's first iteration
        // doesn't double-fire alongside any synchronous startup work.
        t
    });

    let tick_timeout = tick_interval
        .map(|d| d.saturating_mul(2))
        .unwrap_or(HARD_TICK_TIMEOUT_FALLBACK);

    let mut event_rx = event_rx;

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => {
                tracing::debug!(module = %name, "module loop received stop signal");
                break;
            }
            _ = tick_branch(&mut tick_timer) => {
                let result = tokio::time::timeout(tick_timeout, module.tick()).await;
                update_health_after_tick(&name, &health, tick_timeout, result);
            }
            event = event_branch(&mut event_rx) => {
                let Some(event) = event else {
                    // Receiver was dropped (bus removed our subscription).
                    // Disable the event branch but keep ticking.
                    event_rx = None;
                    continue;
                };
                let result = tokio::time::timeout(
                    HARD_EVENT_TIMEOUT,
                    module.on_event(&event),
                )
                .await;
                update_health_after_event(&name, &health, result);
            }
        }
    }

    if let Err(e) = module.stop().await {
        tracing::warn!(module = %name, error = %e, "module stop returned error");
    }
}

async fn tick_branch(timer: &mut Option<tokio::time::Interval>) {
    match timer {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn event_branch(rx: &mut Option<mpsc::Receiver<Event>>) -> Option<Event> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending::<Option<Event>>().await,
    }
}

fn update_health_after_tick(
    name: &str,
    health: &Arc<RwLock<HealthState>>,
    tick_timeout: Duration,
    outcome: Result<ModuleResult<()>, tokio::time::error::Elapsed>,
) {
    let mut guard = health.write().expect("module health lock poisoned");
    match outcome {
        Ok(Ok(())) => {
            if !matches!(*guard, HealthState::Normal) {
                tracing::info!(module = %name, prev = guard.label(), "module recovered to normal");
            }
            *guard = HealthState::Normal;
        }
        Ok(Err(ModuleError::Failed(message))) => {
            tracing::warn!(module = %name, %message, "module tick failed");
            *guard = HealthState::Error {
                since: Instant::now(),
                message,
            };
        }
        Ok(Err(ModuleError::Unavailable(reason))) => {
            tracing::warn!(module = %name, %reason, "module tick reported unavailable");
            *guard = HealthState::Unavailable { reason };
        }
        Err(_elapsed) => {
            let reason = format!("tick exceeded {tick_timeout:?}");
            tracing::warn!(module = %name, %reason, "module tick stale");
            *guard = HealthState::Stale {
                since: Instant::now(),
                reason,
            };
        }
    }
}

fn update_health_after_event(
    name: &str,
    health: &Arc<RwLock<HealthState>>,
    outcome: Result<ModuleResult<()>, tokio::time::error::Elapsed>,
) {
    let mut guard = health.write().expect("module health lock poisoned");
    match outcome {
        Ok(Ok(())) => {
            if !matches!(*guard, HealthState::Normal) {
                tracing::info!(module = %name, prev = guard.label(), "module recovered to normal");
            }
            *guard = HealthState::Normal;
        }
        Ok(Err(ModuleError::Failed(message))) => {
            tracing::warn!(module = %name, %message, "module event handler failed");
            *guard = HealthState::Error {
                since: Instant::now(),
                message,
            };
        }
        Ok(Err(ModuleError::Unavailable(reason))) => {
            tracing::warn!(module = %name, %reason, "module event handler reported unavailable");
            *guard = HealthState::Unavailable { reason };
        }
        Err(_elapsed) => {
            tracing::warn!(module = %name, "module event handler hard-timed out");
            *guard = HealthState::Stale {
                since: Instant::now(),
                reason: format!("event handler exceeded {HARD_EVENT_TIMEOUT:?}"),
            };
        }
    }
}
