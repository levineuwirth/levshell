//! Integration tests for [`levshell_core::ModuleRunner`].

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{
    Event, EventBus, EventKind, HealthState, Module, ModuleError, ModuleResult, ModuleRunner,
};

/// A test module that records every `on_event` call and every `tick`,
/// optionally failing or hanging on either.
struct CountingModule {
    name: String,
    events: Arc<AtomicU32>,
    ticks: Arc<AtomicU32>,
    starts: Arc<AtomicU32>,
    stops: Arc<AtomicU32>,
    tick_interval: Option<Duration>,
    start_outcome: Option<ModuleError>,
    next_tick_outcome: Arc<std::sync::Mutex<Option<ModuleError>>>,
    next_event_outcome: Arc<std::sync::Mutex<Option<ModuleError>>>,
    tick_hang: Arc<std::sync::Mutex<Option<Duration>>>,
}

impl CountingModule {
    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            events: Arc::new(AtomicU32::new(0)),
            ticks: Arc::new(AtomicU32::new(0)),
            starts: Arc::new(AtomicU32::new(0)),
            stops: Arc::new(AtomicU32::new(0)),
            tick_interval: None,
            start_outcome: None,
            next_tick_outcome: Arc::new(std::sync::Mutex::new(None)),
            next_event_outcome: Arc::new(std::sync::Mutex::new(None)),
            tick_hang: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn with_tick(mut self, interval: Duration) -> Self {
        self.tick_interval = Some(interval);
        self
    }

    fn with_start_error(mut self, err: ModuleError) -> Self {
        self.start_outcome = Some(err);
        self
    }
}

#[async_trait]
impl Module for CountingModule {
    fn name(&self) -> &str {
        &self.name
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::WorkspaceChanged,
            EventKind::PowerStateChanged,
        ]
    }

    fn tick_interval(&self) -> Option<Duration> {
        self.tick_interval
    }

    fn channel_capacity(&self) -> usize {
        16
    }

    async fn start(&mut self) -> ModuleResult<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        match self.start_outcome.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn on_event(&mut self, _event: &Event) -> ModuleResult<()> {
        self.events.fetch_add(1, Ordering::SeqCst);
        if let Some(err) = self.next_event_outcome.lock().unwrap().take() {
            return Err(err);
        }
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.ticks.fetch_add(1, Ordering::SeqCst);
        // Peek (don't take) so the hang persists across ticks. Without that,
        // the very next tick after a timeout would succeed and snap the
        // runner back to Normal before the test could observe Stale.
        let hang = { *self.tick_hang.lock().unwrap() };
        if let Some(d) = hang {
            tokio::time::sleep(d).await;
        }
        if let Some(err) = self.next_tick_outcome.lock().unwrap().take() {
            return Err(err);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_starts_module_and_health_is_normal() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus.clone());

    let module = CountingModule::new("counter");
    let starts = module.starts.clone();
    let stops = module.stops.clone();

    runner.register(Box::new(module)).await;
    let handle = runner.find("counter").expect("handle present");
    assert!(matches!(handle.health(), HealthState::Normal));
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    runner.shutdown().await;
    assert_eq!(stops.load(Ordering::SeqCst), 1, "shutdown must call stop()");
}

#[tokio::test]
async fn unavailable_module_is_parked_and_no_task_runs() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus.clone());

    let module = CountingModule::new("missing")
        .with_start_error(ModuleError::Unavailable("not installed".into()));
    let ticks = module.ticks.clone();

    runner.register(Box::new(module)).await;
    let handle = runner.find("missing").unwrap();
    let health = handle.health();
    assert!(matches!(health, HealthState::Unavailable { .. }));
    if let HealthState::Unavailable { reason } = health {
        assert_eq!(reason, "not installed");
    }

    // No subscription should have been created for an unavailable module.
    assert_eq!(bus.subscriber_count(), 0);

    // Even if we publish an event, the module is not running.
    bus.publish(Event::WorkspaceChanged {
        name: "x".into(),
        focused_window: None,
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 0);

    runner.shutdown().await;
}

#[tokio::test]
async fn module_receives_subscribed_events_through_runner() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus.clone());

    let module = CountingModule::new("listener");
    let events = module.events.clone();
    runner.register(Box::new(module)).await;

    // Workspace event — subscribed
    bus.publish(Event::WorkspaceChanged {
        name: "research".into(),
        focused_window: None,
    });
    // Window focused — NOT in subscribed_events, must not increment counter
    bus.publish(Event::WindowFocused {
        app_id: None,
        title: "ignored".into(),
    });
    // Power event — subscribed
    bus.publish(Event::PowerStateChanged { on_battery: false });

    // Give the spawned loop time to drain the channel.
    for _ in 0..50 {
        if events.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(events.load(Ordering::SeqCst), 2);

    let handle = runner.find("listener").unwrap();
    assert!(matches!(handle.health(), HealthState::Normal));

    runner.shutdown().await;
}

#[tokio::test]
async fn tick_fires_on_interval() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus);

    let module = CountingModule::new("ticker").with_tick(Duration::from_millis(20));
    let ticks = module.ticks.clone();
    runner.register(Box::new(module)).await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    let observed = ticks.load(Ordering::SeqCst);
    assert!(
        observed >= 3,
        "expected at least 3 ticks in 120ms with a 20ms interval, saw {observed}"
    );

    let handle = runner.find("ticker").unwrap();
    assert!(matches!(handle.health(), HealthState::Normal));

    runner.shutdown().await;
}

#[tokio::test]
async fn failing_event_handler_transitions_to_error() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus.clone());

    let module = CountingModule::new("flaky");
    let next_event_outcome = module.next_event_outcome.clone();
    runner.register(Box::new(module)).await;

    // Arm the next event call to fail.
    *next_event_outcome.lock().unwrap() = Some(ModuleError::Failed("simulated".into()));

    bus.publish(Event::PowerStateChanged { on_battery: true });

    // Wait for the loop to process and update health
    for _ in 0..50 {
        let h = runner.find("flaky").unwrap().health();
        if matches!(h, HealthState::Error { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let h = runner.find("flaky").unwrap().health();
    match h {
        HealthState::Error { message, .. } => assert_eq!(message, "simulated"),
        other => panic!("expected Error, got {other:?}"),
    }

    // A subsequent successful event recovers to Normal.
    bus.publish(Event::PowerStateChanged { on_battery: false });
    for _ in 0..50 {
        let h = runner.find("flaky").unwrap().health();
        if matches!(h, HealthState::Normal) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(
        runner.find("flaky").unwrap().health(),
        HealthState::Normal
    ));

    runner.shutdown().await;
}

#[tokio::test]
async fn slow_tick_transitions_to_stale() {
    let bus = EventBus::new();
    let mut runner = ModuleRunner::new(bus);

    let module = CountingModule::new("hangs").with_tick(Duration::from_millis(20));
    let tick_hang = module.tick_hang.clone();
    runner.register(Box::new(module)).await;

    // Make the next tick exceed 2× interval (40ms) by sleeping 200ms inside it.
    *tick_hang.lock().unwrap() = Some(Duration::from_millis(200));

    // Wait long enough for the timeout to fire and the health to flip.
    for _ in 0..40 {
        let h = runner.find("hangs").unwrap().health();
        if matches!(h, HealthState::Stale { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        matches!(runner.find("hangs").unwrap().health(), HealthState::Stale { .. }),
        "module should be Stale after a tick exceeds 2× the interval"
    );

    runner.shutdown().await;
}
