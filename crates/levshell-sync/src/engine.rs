//! [`SyncEngine`] — schedules sync adapters and publishes lifecycle events.
//!
//! The engine owns a registry of adapters (`Arc<dyn SyncAdapter>`) and one
//! task per adapter. Each task loops: probe → sync → sleep, publishing
//! `SyncCompleted` on success, `SyncError` on failure, and `SyncConflict`
//! for each entity flagged during apply.
//!
//! # Power-awareness (spec §5.3)
//!
//! When the system is on battery, each adapter's poll interval is
//! multiplied by [`SyncEngineConfig::battery_poll_multiplier`] (default
//! `2.0`). The current battery state is broadcast through a
//! [`tokio::sync::watch`] channel updated by a dedicated watcher task
//! that subscribes to [`levshell_core::Event::PowerStateChanged`] on the
//! bus. When the state flips, every in-flight `sleep` in every adapter
//! task is woken so the new multiplier takes effect immediately —
//! **not** on the next sleep. An in-flight `sync()` call is never
//! interrupted; the change applies to the post-sync sleep.
//!
//! The engine deliberately does NOT manage `sync_metadata` writes — that
//! lives inside each adapter because only the adapter knows which entity
//! table to upsert. The engine's role is scheduling + observability.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use levshell_core::{Event, EventBus, EventKind};
use levshell_data::DataStore;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::adapter::{SyncAdapter, SyncContext, SyncError, SyncStatus};

/// Engine configuration applied uniformly across adapters. The current
/// on-battery state is NOT stored here — it lives in a watch channel
/// updated by a dedicated task in response to
/// [`Event::PowerStateChanged`] events on the bus. Pass an
/// `initial_on_battery` value to [`SyncEngine::with_config`] if the
/// caller knows the state at boot.
#[derive(Debug, Clone)]
pub struct SyncEngineConfig {
    /// Multiplier applied to every adapter's poll interval when the
    /// system is on battery. Defaults to `2.0` per spec §5.3.
    pub battery_poll_multiplier: f32,
    /// Initial value for the on-battery flag. Subsequent updates come
    /// from `PowerStateChanged` events on the bus.
    pub initial_on_battery: bool,
}

impl Default for SyncEngineConfig {
    fn default() -> Self {
        Self {
            battery_poll_multiplier: 2.0,
            initial_on_battery: false,
        }
    }
}

/// Handle to a running sync engine. Dropping all handles stops the adapter
/// tasks (via the shutdown channel) but waits for in-flight syncs to finish
/// gracefully.
pub struct SyncEngineHandle {
    tasks: Vec<JoinHandle<()>>,
    shutdown_tx: Vec<oneshot::Sender<()>>,
    battery_watcher: Option<JoinHandle<()>>,
}

impl SyncEngineHandle {
    /// Signal all adapter loops to stop after their current sync (or sleep)
    /// completes, then await their join handles.
    pub async fn shutdown(self) {
        for tx in self.shutdown_tx {
            let _ = tx.send(());
        }
        for task in self.tasks {
            let _ = task.await;
        }
        if let Some(w) = self.battery_watcher {
            w.abort();
            let _ = w.await;
        }
    }

    /// How many adapter tasks are being managed.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// Registry + scheduler for sync adapters. Build with [`Self::new`],
/// register adapters via [`Self::register`], then call [`Self::spawn`] to
/// launch the per-adapter tasks.
pub struct SyncEngine {
    store: DataStore,
    bus: EventBus,
    config: SyncEngineConfig,
    adapters: Vec<Arc<dyn SyncAdapter>>,
}

impl SyncEngine {
    pub fn new(store: DataStore, bus: EventBus) -> Self {
        Self::with_config(store, bus, SyncEngineConfig::default())
    }

    pub fn with_config(store: DataStore, bus: EventBus, config: SyncEngineConfig) -> Self {
        Self {
            store,
            bus,
            config,
            adapters: Vec::new(),
        }
    }

    /// Register an adapter. Adapters must be registered before [`Self::spawn`]
    /// is called; later registration is not supported (would require a
    /// different control-plane design).
    pub fn register(&mut self, adapter: Arc<dyn SyncAdapter>) {
        self.adapters.push(adapter);
    }

    /// Launch a task per registered adapter. Consumes `self` — the engine's
    /// state is moved into the tasks, and further configuration is via
    /// bus events.
    pub fn spawn(self) -> SyncEngineHandle {
        // Shared battery state backed by a `watch` channel so that (1)
        // adapter tasks read the current value without locking, and (2)
        // an in-flight sleep can be interrupted as soon as the state
        // flips — the adapter recomputes and restarts its sleep with
        // the new interval.
        let (battery_tx, battery_rx) = watch::channel(self.config.initial_on_battery);
        let battery_watcher = spawn_battery_watcher(self.bus.clone(), battery_tx);

        let mut tasks = Vec::with_capacity(self.adapters.len());
        let mut shutdown_tx = Vec::with_capacity(self.adapters.len());
        for adapter in self.adapters {
            let (tx, rx) = oneshot::channel();
            shutdown_tx.push(tx);
            let store = self.store.clone();
            let bus = self.bus.clone();
            let config = self.config.clone();
            let battery_rx = battery_rx.clone();
            let handle =
                tokio::spawn(adapter_loop(adapter, store, bus, config, battery_rx, rx));
            tasks.push(handle);
        }
        SyncEngineHandle {
            tasks,
            shutdown_tx,
            battery_watcher: Some(battery_watcher),
        }
    }
}

/// Background task that updates the shared battery watch channel
/// whenever a [`Event::PowerStateChanged`] fires on the bus. Lives for
/// as long as the engine. Aborted during [`SyncEngineHandle::shutdown`].
fn spawn_battery_watcher(bus: EventBus, battery_tx: watch::Sender<bool>) -> JoinHandle<()> {
    // Capacity 8: PowerStateChanged is rare (AC plug / unplug), but give
    // some headroom so a storm of spurious events doesn't get us dropped
    // by the bus.
    let mut rx = bus.subscribe("sync-engine-battery-watcher", [EventKind::PowerStateChanged], 8);
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Event::PowerStateChanged { on_battery: new_state } = event {
                // send_if_modified suppresses the notification when the
                // value didn't actually change, so spurious duplicate
                // events don't kick adapter loops awake for no reason.
                let changed = battery_tx.send_if_modified(|current| {
                    if *current != new_state {
                        *current = new_state;
                        true
                    } else {
                        false
                    }
                });
                if changed {
                    tracing::info!(on_battery = new_state, "sync engine updated battery mode");
                }
            }
        }
    })
}

async fn adapter_loop(
    adapter: Arc<dyn SyncAdapter>,
    store: DataStore,
    bus: EventBus,
    config: SyncEngineConfig,
    mut battery_rx: watch::Receiver<bool>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let name = adapter.name().to_string();
    let mut since = None;

    tracing::info!(provider = %name, "sync adapter loop started");

    loop {
        // Probe first — if the adapter is unavailable, skip the sync but
        // keep the loop alive. A re-install or re-configure between ticks
        // should recover automatically on the next probe.
        let ctx = SyncContext {
            store: store.clone(),
            since,
        };
        let status = adapter.probe(&ctx).await;
        match status {
            SyncStatus::Unavailable => {
                tracing::debug!(provider = %name, "adapter unavailable, skipping sync");
            }
            _ => {
                run_sync_once(&adapter, &ctx, &bus, &name, &mut since).await;
            }
        }

        // Sleep until the next tick, interruptible by battery flips and
        // shutdown. On battery change we fall back through the loop
        // without performing another sync — we just reschedule the
        // sleep with the new interval. This matches the spec's intent
        // ("poll intervals are multiplied when on battery") while
        // avoiding a spurious sync every time the user plugs/unplugs.
        if !interruptible_sleep(&adapter, &config, &mut battery_rx, &mut shutdown_rx).await {
            tracing::info!(provider = %name, "sync adapter loop shutting down");
            return;
        }
    }
}

/// Sleep for the current effective interval, restarting whenever the
/// battery state flips (so the new multiplier takes effect immediately).
/// Returns `true` on natural completion, `false` if shutdown was
/// requested during the sleep.
async fn interruptible_sleep(
    adapter: &Arc<dyn SyncAdapter>,
    config: &SyncEngineConfig,
    battery_rx: &mut watch::Receiver<bool>,
    shutdown_rx: &mut oneshot::Receiver<()>,
) -> bool {
    loop {
        let on_battery = *battery_rx.borrow();
        let interval = effective_interval(adapter, config, on_battery);
        tokio::select! {
            _ = sleep(interval) => return true,
            changed = battery_rx.changed() => {
                if changed.is_err() {
                    // Watcher sender dropped; engine is shutting down.
                    return false;
                }
                // Loop around and recompute with the new state.
                continue;
            }
            _ = &mut *shutdown_rx => return false,
        }
    }
}

async fn run_sync_once(
    adapter: &Arc<dyn SyncAdapter>,
    ctx: &SyncContext,
    bus: &EventBus,
    name: &str,
    since: &mut Option<chrono::DateTime<chrono::Utc>>,
) {
    let start = Instant::now();
    let result = timeout(adapter.timeout(), adapter.sync(ctx)).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    match result {
        Err(_) => {
            tracing::warn!(provider = %name, "sync timed out");
            bus.publish(Event::SyncError {
                provider: name.to_string(),
                error: SyncError::Timeout.to_string(),
            });
        }
        Ok(Err(err)) => {
            tracing::warn!(provider = %name, error = %err, "sync failed");
            bus.publish(Event::SyncError {
                provider: name.to_string(),
                error: err.to_string(),
            });
        }
        Ok(Ok(report)) => {
            // Advance the cursor only on success. On failure/timeout we
            // re-try the same window next tick rather than skipping past
            // entities that never made it into the store.
            *since = Some(Utc::now());
            let conflict_count = report.conflicts.len() as u32;
            for conflict in &report.conflicts {
                bus.publish(Event::SyncConflict {
                    provider: name.to_string(),
                    entity_type: conflict.entity_type.as_str().to_string(),
                    external_id: conflict.external_id.clone(),
                    reason: conflict.reason.clone(),
                });
            }
            tracing::info!(
                provider = %name,
                upserted = report.upserted,
                deleted = report.deleted,
                conflicts = conflict_count,
                elapsed_ms,
                "sync completed"
            );
            bus.publish(Event::SyncCompleted {
                provider: name.to_string(),
                upserted: report.upserted,
                deleted: report.deleted,
                conflicts: conflict_count,
                duration_ms: elapsed_ms,
            });
        }
    }
}

fn effective_interval(
    adapter: &Arc<dyn SyncAdapter>,
    config: &SyncEngineConfig,
    on_battery: bool,
) -> std::time::Duration {
    let base = adapter.poll_interval();
    if on_battery && config.battery_poll_multiplier > 1.0 {
        let millis = base.as_millis() as f32 * config.battery_poll_multiplier;
        std::time::Duration::from_millis(millis.round() as u64)
    } else {
        base
    }
}
