//! Integration tests for `levshell_modules::IdeationModule`.
//!
//! Builds a real in-memory data store + project registry, wires them
//! through the module, seeds the RNG deterministically, and drives one
//! tick via `tick_for_test()`. Asserts that `Event::NudgeDelivered`
//! lands on the bus with the expected project/kind.

use std::time::Duration;

use levshell_core::{Event, EventBus, EventKind};
use levshell_data::DataStore;
use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec, WriterTask};
use levshell_modules::{IdeationConfig, IdeationModule, NudgeWeights};
use levshell_projects::ProjectRegistry;
use tempfile::TempDir;
use tokio::io::duplex;
use tokio::time::timeout;

async fn writer_task() -> WriterTask {
    let (a, _b) = duplex(4096);
    let writer = IpcWriter::from_parts(a, JsonCodec);
    spawn_writer_task(writer, 16)
}

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.expect("open store")
}

fn write_project_toml(dir: &TempDir, body: &str) {
    std::fs::write(dir.path().join("p.toml"), body).unwrap();
}

/// Config forcing a nudge every tick: lambda 1 minute, tick 60 s → p=1.
/// All weight on OpenQuestion so the nudge kind is deterministic given
/// the test project's open_questions.
fn deterministic_config() -> IdeationConfig {
    IdeationConfig {
        enabled: true,
        lambda_minutes: 1.0,
        tick_secs: 60,
        weights: NudgeWeights {
            open_question: 1.0,
            cross_connection: 0.0,
            blocked: 0.0,
        },
        blocked_escalation_factor: 1.0,
        stale_project_hours: 24,
        recent_seed_hours: 6,
    }
}

async fn subscribe(bus: &EventBus) -> tokio::sync::mpsc::Receiver<Event> {
    bus.subscribe("ideation-test", [EventKind::NudgeDelivered], 16)
}

#[tokio::test]
async fn tick_without_registry_is_noop() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = subscribe(&bus).await;
    let writer = writer_task().await;

    let module =
        IdeationModule::with_config(bus.clone(), writer.publisher, store, None, deterministic_config())
            .with_seeded_rng(1);

    // Even with p=1 and all-open-question weight, no registry ⇒ no nudge.
    module.tick_for_test().await.unwrap();
    assert!(
        timeout(Duration::from_millis(50), rx.recv()).await.is_err(),
        "no registry → no NudgeDelivered"
    );
}

#[tokio::test]
async fn tick_without_projects_is_noop() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = subscribe(&bus).await;
    let writer = writer_task().await;

    let dir = TempDir::new().unwrap();
    let registry =
        ProjectRegistry::load_from_dir(store.clone(), bus.clone(), dir.path())
            .await
            .unwrap();

    let module = IdeationModule::with_config(
        bus.clone(),
        writer.publisher,
        store,
        Some(registry),
        deterministic_config(),
    )
    .with_seeded_rng(2);

    module.tick_for_test().await.unwrap();
    assert!(
        timeout(Duration::from_millis(50), rx.recv()).await.is_err(),
        "no projects → no NudgeDelivered"
    );
}

#[tokio::test]
async fn tick_publishes_open_question_nudge() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = subscribe(&bus).await;
    let writer = writer_task().await;

    let dir = TempDir::new().unwrap();
    write_project_toml(
        &dir,
        r#"
name = "Levshell"
status = "active"
description = "Context shell"
open_questions = ["What's the next milestone?"]
tags = ["rust"]
"#,
    );
    let registry =
        ProjectRegistry::load_from_dir(store.clone(), bus.clone(), dir.path())
            .await
            .unwrap();
    assert_eq!(registry.list().await.len(), 1);

    let module = IdeationModule::with_config(
        bus.clone(),
        writer.publisher,
        store,
        Some(registry),
        deterministic_config(),
    )
    .with_seeded_rng(99);

    module.tick_for_test().await.unwrap();

    let event = timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("NudgeDelivered within 500ms")
        .expect("bus closed unexpectedly");
    match event {
        Event::NudgeDelivered { kind, title, .. } => {
            assert_eq!(kind, "open_question");
            assert_eq!(title, "Levshell");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn disabled_config_skips_tick() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = subscribe(&bus).await;
    let writer = writer_task().await;

    let dir = TempDir::new().unwrap();
    write_project_toml(
        &dir,
        r#"
name = "Levshell"
status = "active"
description = ""
open_questions = ["q"]
tags = []
"#,
    );
    let registry =
        ProjectRegistry::load_from_dir(store.clone(), bus.clone(), dir.path())
            .await
            .unwrap();

    let mut cfg = deterministic_config();
    cfg.enabled = false;
    let module = IdeationModule::with_config(
        bus.clone(),
        writer.publisher,
        store,
        Some(registry),
        cfg,
    )
    .with_seeded_rng(3);

    module.tick_for_test().await.unwrap();
    assert!(
        timeout(Duration::from_millis(50), rx.recv()).await.is_err(),
        "disabled → no NudgeDelivered"
    );
}

#[tokio::test]
async fn blocked_project_escalation_fires_with_blocked_weight() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let mut rx = subscribe(&bus).await;
    let writer = writer_task().await;

    let dir = TempDir::new().unwrap();
    write_project_toml(
        &dir,
        r#"
name = "Stuck"
status = "blocked"
description = ""
open_questions = []
tags = []
"#,
    );
    let registry =
        ProjectRegistry::load_from_dir(store.clone(), bus.clone(), dir.path())
            .await
            .unwrap();

    let cfg = IdeationConfig {
        weights: NudgeWeights {
            open_question: 0.0,
            cross_connection: 0.0,
            blocked: 1.0,
        },
        ..deterministic_config()
    };
    let module = IdeationModule::with_config(
        bus.clone(),
        writer.publisher,
        store,
        Some(registry),
        cfg,
    )
    .with_seeded_rng(5);

    module.tick_for_test().await.unwrap();
    let event = timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("NudgeDelivered within 500ms")
        .expect("bus closed unexpectedly");
    match event {
        Event::NudgeDelivered { kind, title, .. } => {
            assert_eq!(kind, "blocked_escalation");
            assert_eq!(title, "Stuck");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

