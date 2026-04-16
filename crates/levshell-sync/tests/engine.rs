//! Integration tests for `levshell_sync::SyncEngine`.
//!
//! A mock adapter exercises the engine end-to-end: registration, per-tick
//! probe, sync success + failure paths, event publication on the bus,
//! shutdown via the handle, and battery-mode interval multiplication. The
//! tests intentionally rely on tokio's pause/advance time facility so they
//! run deterministically without sleeping real time.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{EventBus, EventKind};
use levshell_data::{DataStore, EntityType, ListNotes, NewNote};
use levshell_sync::{
    SyncAdapter, SyncContext, SyncEngine, SyncEngineConfig, SyncError, SyncReport, SyncStatus,
};

/// Mock adapter that inserts one note per sync call. Exposes counters so
/// tests can assert how many ticks actually executed.
struct MockAdapter {
    name: &'static str,
    poll_interval: Duration,
    probe_calls: Arc<AtomicU32>,
    sync_calls: Arc<AtomicU32>,
    /// If `Some`, the next `sync()` call returns this error once (then
    /// resets to `None`). Used to test the `SyncError` bus event.
    inject_error: std::sync::Mutex<Option<String>>,
    /// If `true`, `probe()` reports `Unavailable` and `sync()` is skipped.
    unavailable: bool,
}

impl MockAdapter {
    fn new(name: &'static str, poll_interval: Duration) -> Self {
        Self {
            name,
            poll_interval,
            probe_calls: Arc::new(AtomicU32::new(0)),
            sync_calls: Arc::new(AtomicU32::new(0)),
            inject_error: std::sync::Mutex::new(None),
            unavailable: false,
        }
    }
}

#[async_trait]
impl SyncAdapter for MockAdapter {
    fn name(&self) -> &str {
        self.name
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Note]
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            SyncStatus::Unavailable
        } else {
            SyncStatus::Healthy
        }
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport, SyncError> {
        let n = self.sync_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(msg) = self.inject_error.lock().unwrap().take() {
            return Err(SyncError::External(msg));
        }
        // Insert a deterministic note so the test can observe the engine
        // drove apply end-to-end, not just that it called sync().
        ctx.store
            .insert_note(NewNote {
                title: format!("mock-{}-{n}", self.name),
                content: "inserted by mock adapter".into(),
                project_id: None,
            })
            .await?;
        Ok(SyncReport {
            upserted: 1,
            deleted: 0,
            conflicts: vec![],
        })
    }
}

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.expect("open store")
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn engine_runs_adapter_and_publishes_completed_event() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = bus.subscribe(
        "test-observer",
        [EventKind::SyncCompleted, EventKind::SyncError],
        16,
    );

    let adapter = Arc::new(MockAdapter::new("mock", Duration::from_secs(60)));
    let sync_calls = adapter.sync_calls.clone();

    let mut engine = SyncEngine::new(store.clone(), bus);
    engine.register(adapter.clone());
    let handle = engine.spawn();

    // Yield until the first sync completes — the loop calls sync() before
    // its first sleep, so the initial pass executes without needing to
    // advance tokio time.
    let first_event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("waited too long for first SyncCompleted event")
        .expect("bus closed unexpectedly");
    match first_event {
        levshell_core::Event::SyncCompleted {
            provider,
            upserted,
            deleted,
            conflicts,
            ..
        } => {
            assert_eq!(provider, "mock");
            assert_eq!(upserted, 1);
            assert_eq!(deleted, 0);
            assert_eq!(conflicts, 0);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(sync_calls.load(Ordering::SeqCst), 1);

    // Advance past the adapter's poll interval; expect a second tick.
    tokio::time::advance(Duration::from_secs(61)).await;
    let second_event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("waited too long for second SyncCompleted event")
        .expect("bus closed");
    assert!(matches!(
        second_event,
        levshell_core::Event::SyncCompleted { ref provider, .. } if provider == "mock"
    ));
    assert_eq!(sync_calls.load(Ordering::SeqCst), 2);

    // The data store should have received the mock adapter's upserts.
    let notes = store.list_notes(ListNotes::default()).await.unwrap();
    assert_eq!(notes.len(), 2, "both syncs should have inserted a note");

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn engine_publishes_sync_error_on_failure() {
    let bus = EventBus::new();
    let store = fresh_store().await;
    let mut rx = bus.subscribe(
        "test-observer",
        [EventKind::SyncError, EventKind::SyncCompleted],
        16,
    );

    let adapter = Arc::new(MockAdapter::new("flaky", Duration::from_secs(30)));
    *adapter.inject_error.lock().unwrap() = Some("mocked failure".into());

    let mut engine = SyncEngine::new(store, bus);
    engine.register(adapter.clone());
    let handle = engine.spawn();

    let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for SyncError event")
        .expect("bus closed");
    match event {
        levshell_core::Event::SyncError { provider, error } => {
            assert_eq!(provider, "flaky");
            assert!(error.contains("mocked failure"));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // After the injected error, subsequent ticks recover.
    tokio::time::advance(Duration::from_secs(31)).await;
    let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for recovery")
        .expect("bus closed");
    assert!(matches!(event, levshell_core::Event::SyncCompleted { .. }));

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn engine_skips_sync_when_adapter_unavailable() {
    let bus = EventBus::new();
    let store = fresh_store().await;
    let mut rx = bus.subscribe(
        "test-observer",
        [EventKind::SyncCompleted, EventKind::SyncError],
        16,
    );

    let mut adapter = MockAdapter::new("missing", Duration::from_secs(30));
    adapter.unavailable = true;
    let adapter = Arc::new(adapter);
    let sync_calls = adapter.sync_calls.clone();

    let mut engine = SyncEngine::new(store, bus);
    engine.register(adapter);
    let handle = engine.spawn();

    // Advance several intervals; no sync-related events should appear.
    tokio::time::advance(Duration::from_secs(120)).await;
    let event = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        event.is_err(),
        "no sync events should be published for unavailable adapter"
    );
    assert_eq!(
        sync_calls.load(Ordering::SeqCst),
        0,
        "sync() must not be called when probe reports Unavailable"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn battery_mode_lengthens_poll_interval() {
    let bus = EventBus::new();
    let store = fresh_store().await;
    let mut rx = bus.subscribe("test-observer", [EventKind::SyncCompleted], 16);

    let adapter = Arc::new(MockAdapter::new("batt", Duration::from_secs(60)));
    let sync_calls = adapter.sync_calls.clone();

    let mut engine = SyncEngine::with_config(
        store,
        bus,
        SyncEngineConfig {
            battery_poll_multiplier: 3.0,
            on_battery: true,
        },
    );
    engine.register(adapter);
    let handle = engine.spawn();

    // Consume the first tick (fires immediately before the first sleep).
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout on first tick")
        .expect("bus closed");
    assert_eq!(sync_calls.load(Ordering::SeqCst), 1);

    // Advance only 60s — on battery with a 3× multiplier the next tick
    // should be at 180s, so no second event yet.
    tokio::time::advance(Duration::from_secs(60)).await;
    let second = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(second.is_err(), "second tick must wait the multiplied interval");
    assert_eq!(sync_calls.load(Ordering::SeqCst), 1);

    // Advance the remaining 120s to reach 180s total; now the second tick fires.
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout on second tick after multiplied interval")
        .expect("bus closed");
    assert_eq!(sync_calls.load(Ordering::SeqCst), 2);

    handle.shutdown().await;
}
