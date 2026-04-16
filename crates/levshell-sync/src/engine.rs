//! [`SyncEngine`] — schedules sync adapters and publishes lifecycle events.
//!
//! The engine owns a registry of adapters (`Arc<dyn SyncAdapter>`) and one
//! task per adapter. Each task loops: probe → sync → sleep, publishing
//! `SyncCompleted` on success, `SyncError` on failure, and `SyncConflict`
//! for each entity flagged during apply. Battery mode (per spec §5.3)
//! multiplies the adapter's poll interval by a configurable factor —
//! delegated to the caller for now via [`SyncEngineConfig`].
//!
//! The engine deliberately does NOT manage `sync_metadata` writes — that
//! lives inside each adapter because only the adapter knows which entity
//! table to upsert. The engine's role is scheduling + observability.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use levshell_core::{Event, EventBus};
use levshell_data::DataStore;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::adapter::{SyncAdapter, SyncContext, SyncError, SyncStatus};

/// Engine configuration applied uniformly across adapters.
#[derive(Debug, Clone)]
pub struct SyncEngineConfig {
    /// Multiplier applied to every adapter's poll interval when
    /// [`Self::on_battery`] is true. Defaults to `2.0` per spec §5.3.
    pub battery_poll_multiplier: f32,
    /// Whether the engine should currently treat the system as on battery.
    /// Typically wired to the `PowerStateChanged` event on the bus.
    pub on_battery: bool,
}

impl Default for SyncEngineConfig {
    fn default() -> Self {
        Self {
            battery_poll_multiplier: 2.0,
            on_battery: false,
        }
    }
}

/// Handle to a running sync engine. Dropping all handles stops the adapter
/// tasks (via the shutdown channel) but waits for in-flight syncs to finish
/// gracefully.
pub struct SyncEngineHandle {
    tasks: Vec<JoinHandle<()>>,
    shutdown_tx: Vec<oneshot::Sender<()>>,
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
    /// bus events (future work).
    pub fn spawn(self) -> SyncEngineHandle {
        let mut tasks = Vec::with_capacity(self.adapters.len());
        let mut shutdown_tx = Vec::with_capacity(self.adapters.len());
        for adapter in self.adapters {
            let (tx, rx) = oneshot::channel();
            shutdown_tx.push(tx);
            let store = self.store.clone();
            let bus = self.bus.clone();
            let config = self.config.clone();
            let handle = tokio::spawn(adapter_loop(adapter, store, bus, config, rx));
            tasks.push(handle);
        }
        SyncEngineHandle { tasks, shutdown_tx }
    }
}

async fn adapter_loop(
    adapter: Arc<dyn SyncAdapter>,
    store: DataStore,
    bus: EventBus,
    config: SyncEngineConfig,
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

        // Sleep until the next tick, honouring battery mode. The sleep is
        // interruptible by the shutdown signal so tests and clean
        // shutdowns do not have to wait out the full interval.
        let interval = effective_interval(&adapter, &config);
        tokio::select! {
            _ = sleep(interval) => {}
            _ = &mut shutdown_rx => {
                tracing::info!(provider = %name, "sync adapter loop shutting down");
                return;
            }
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
) -> std::time::Duration {
    let base = adapter.poll_interval();
    if config.on_battery && config.battery_poll_multiplier > 1.0 {
        let millis = base.as_millis() as f32 * config.battery_poll_multiplier;
        std::time::Duration::from_millis(millis.round() as u64)
    } else {
        base
    }
}
