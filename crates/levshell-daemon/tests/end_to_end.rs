//! End-to-end test for `levshell_daemon::run`.
//!
//! Spins up the real daemon library against a tempfile-backed database and
//! socket, registers a fake module that publishes a known WidgetUpdate the
//! moment it starts, then connects a UnixStream "shell" client and verifies
//! the message arrives over the wire. Drives shutdown via a oneshot.
//!
//! This test exercises the full vertical slice without needing Sway: it
//! proves that the data store opens, the IPC server binds, the writer task
//! routes messages from a registered module's publisher to the socket, and
//! the runner shuts down cleanly when signaled.

use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult};
use levshell_daemon::{run, run_with_sync, DaemonConfig, ModuleFactory, SyncAdapterFactory};
use levshell_data::{DataStore, ListNotes};
use levshell_sync::{ObsidianAdapter, ObsidianConfig, SyncAdapter};
use levshell_ipc::{
    BarDensity, ClientRole, CtlRequest, CtlResponse, DaemonMessage, Hello, IpcConnection,
    JsonCodec, ShellMessage, WidgetAction, WidgetPublisher, WidgetStatus, WidgetUpdate,
};
use levshell_modules::{
    default_context_engine, default_widgets, MemoryModule, PaletteModule, PaletteProvider,
};
use serde_json::json;
use tokio::net::UnixStream;
use tokio::sync::oneshot;

/// Helper: connect a shell client, send the Hello handshake, and return the
/// ready-to-use IpcConnection.
async fn connect_shell(socket_path: &std::path::Path) -> IpcConnection<JsonCodec> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("shell connect");
    let mut conn = IpcConnection::<JsonCodec>::from_unix_stream(stream);
    conn.writer()
        .send(&Hello::new(ClientRole::Shell))
        .await
        .expect("send shell Hello");
    conn
}

/// Helper: connect a ctl client, send the Hello handshake, send a request,
/// read one response, and return it.
async fn ctl_round_trip(
    socket_path: &std::path::Path,
    request: CtlRequest,
) -> CtlResponse {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("ctl connect");
    let mut conn = IpcConnection::<JsonCodec>::from_unix_stream(stream);
    conn.writer()
        .send(&Hello::new(ClientRole::Ctl))
        .await
        .expect("send ctl Hello");
    conn.writer().send(&request).await.expect("send ctl request");
    conn.reader().recv().await.expect("recv ctl response")
}

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..50 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never bound socket at {}", path.display());
}

/// Test-only module that publishes a single WidgetUpdate the moment `start`
/// fires and also publishes a bus event so we can validate the bus pathway
/// indirectly via the runner's bookkeeping.
struct FakeModule {
    publisher: WidgetPublisher,
    bus: EventBus,
}

#[async_trait]
impl Module for FakeModule {
    fn name(&self) -> &str {
        "fake-test"
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        Vec::new()
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let msg = DaemonMessage::WidgetUpdate(WidgetUpdate {
            widget_id: "workspace-indicator".into(),
            widget_type: "workspace_indicator".into(),
            state: json!({
                "workspaces": [
                    { "name": "research", "num": 1, "focused": true, "urgent": false, "output": "eDP-1" }
                ],
                "active": "research",
                "focused_window": null
            }),
            status: WidgetStatus::Normal,
        });
        self.publisher
            .send(msg)
            .await
            .expect("publisher send during start");
        self.bus.publish(Event::WorkspaceChanged {
            name: "research".into(),
            focused_window: None,
        });
        Ok(())
    }
}

fn temp_paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("levshell.db");
    let socket_path = dir.path().join("levshell.sock");
    (dir, db_path, socket_path)
}

#[tokio::test]
async fn daemon_publishes_widget_update_over_ipc_to_a_unixstream_shell() {
    let (_dir, db_path, socket_path) = temp_paths();

    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
            projects_dir: None,
        themes_dir: None,
    };

    let factory: ModuleFactory = Box::new(|bus, publisher, _store, _projects| {
        vec![Box::new(FakeModule { bus, publisher }) as Box<dyn Module>]
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });

    // Spawn the daemon. It will block on accept() until the client below
    // connects.
    let daemon_handle = tokio::spawn(async move { run(config, factory, shutdown).await });

    wait_for_socket(&socket_path).await;

    // Connect as the "shell" with the Hello handshake.
    let mut shell_conn = connect_shell(&socket_path).await;

    // The fake module publishes its WidgetUpdate during start(), which
    // happens after the daemon accept()s our connection above. We expect
    // exactly one widget_update on the wire.
    let received: DaemonMessage = tokio::time::timeout(
        Duration::from_secs(2),
        shell_conn.reader().recv(),
    )
    .await
    .expect("recv timeout")
    .expect("recv");
    match received {
        DaemonMessage::WidgetUpdate(update) => {
            assert_eq!(update.widget_id, "workspace-indicator");
            assert_eq!(update.status, WidgetStatus::Normal);
            assert_eq!(
                update.state.get("active").and_then(|v| v.as_str()),
                Some("research")
            );
        }
        other => panic!("expected WidgetUpdate, got {other:?}"),
    }

    // Send a ShellMessage to exercise the reader path; the daemon just logs
    // it. We don't assert anything here — the reader task is fire-and-log.
    shell_conn
        .writer()
        .send(&ShellMessage::WidgetAction(WidgetAction {
            widget_id: "workspace-indicator".into(),
            action: "click".into(),
            data: json!(null),
        }))
        .await
        .unwrap();

    // Drive shutdown.
    let _ = shutdown_tx.send(());

    // Daemon should exit cleanly within a couple seconds.
    let result = tokio::time::timeout(Duration::from_secs(3), daemon_handle)
        .await
        .expect("daemon to exit")
        .expect("daemon task to join");
    result.expect("daemon to return Ok");

    // The IpcServer's Drop should have unlinked the socket file.
    assert!(!socket_path.exists(), "socket file should be unlinked on shutdown");
}

// ---------------------------------------------------------------------------
// Multi-client tests (Phase 1.1)
// ---------------------------------------------------------------------------

fn empty_factory() -> ModuleFactory {
    Box::new(|_bus, _publisher, _store, _projects| Vec::new())
}

/// Boot the daemon with a given factory, return the daemon task handle, the
/// shutdown sender, and the socket path. The caller is responsible for
/// triggering shutdown and joining the handle.
async fn boot_daemon(
    factory: ModuleFactory,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    oneshot::Sender<()>,
) {
    let (dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
            projects_dir: None,
        themes_dir: None,
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;
    (dir, socket_path, handle, shutdown_tx)
}

#[tokio::test]
async fn ctl_ping_round_trips_before_any_shell() {
    let (_dir, socket_path, handle, shutdown_tx) = boot_daemon(empty_factory()).await;

    let response = ctl_round_trip(&socket_path, CtlRequest::Ping).await;
    assert!(matches!(response, CtlResponse::Pong));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn ctl_status_reflects_shell_connection_state() {
    let (_dir, socket_path, handle, shutdown_tx) = boot_daemon(empty_factory()).await;

    // No shell yet — status should say shell_connected = false.
    let response = ctl_round_trip(&socket_path, CtlRequest::Status).await;
    let snapshot = match response {
        CtlResponse::Status(s) => s,
        other => panic!("expected Status, got {other:?}"),
    };
    assert!(!snapshot.shell_connected);
    assert_eq!(snapshot.module_count, 0);
    assert_eq!(snapshot.protocol_version, levshell_ipc::PROTOCOL_VERSION);

    // Connect a shell.
    let _shell_conn = connect_shell(&socket_path).await;
    // Give the daemon a tick to finalize the shell setup.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let response = ctl_round_trip(&socket_path, CtlRequest::Status).await;
    let snapshot = match response {
        CtlResponse::Status(s) => s,
        other => panic!("expected Status, got {other:?}"),
    };
    assert!(snapshot.shell_connected);

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn ctl_density_change_publishes_bus_event() {
    use levshell_ipc::BarDensity;

    let (_dir, socket_path, handle, shutdown_tx) = boot_daemon(empty_factory()).await;

    // Subscribe a test observer on the bus BEFORE sending the ctl request.
    // Can't subscribe through the daemon externally, so we drive this via a
    // FakeModule-style factory in a follow-up test instead.
    //
    // This test just verifies the round-trip returns Ok — the bus event is
    // observed by the dedicated bus-subscriber test below.
    let response = ctl_round_trip(
        &socket_path,
        CtlRequest::Density {
            mode: BarDensity::Compact,
        },
    )
    .await;
    assert!(matches!(response, CtlResponse::Ok));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn second_shell_connection_is_rejected() {
    let (_dir, socket_path, handle, shutdown_tx) = boot_daemon(empty_factory()).await;

    // First shell gets in.
    let _shell = connect_shell(&socket_path).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Second shell attempts to connect. The daemon sends a CtlResponse::Error
    // and closes. We read the error back on the shell-facing reader.
    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("second shell connect");
    let mut second = IpcConnection::<JsonCodec>::from_unix_stream(stream);
    second
        .writer()
        .send(&Hello::new(ClientRole::Shell))
        .await
        .expect("send Hello");

    let response: CtlResponse = tokio::time::timeout(
        Duration::from_secs(2),
        second.reader().recv(),
    )
    .await
    .expect("recv within 2s")
    .expect("recv ok");
    assert!(
        matches!(response, CtlResponse::Error { .. }),
        "expected rejection, got {response:?}"
    );

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn ctl_and_shell_coexist_with_shell_receiving_updates() {
    // End-to-end: shell connects, fake module publishes a WidgetUpdate, ctl
    // client queries status while the shell is attached, the shell still
    // receives its update without interference.
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
            projects_dir: None,
        themes_dir: None,
    };
    let factory: ModuleFactory = Box::new(|bus, publisher, _store, _projects| {
        vec![Box::new(FakeModule { bus, publisher }) as Box<dyn Module>]
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;

    let mut shell_conn = connect_shell(&socket_path).await;

    // Intermix a ctl Status request before reading the shell's update.
    let status = ctl_round_trip(&socket_path, CtlRequest::Status).await;
    assert!(matches!(status, CtlResponse::Status(_)));

    // Shell should still receive the widget update from the fake module.
    let received: DaemonMessage = tokio::time::timeout(
        Duration::from_secs(2),
        shell_conn.reader().recv(),
    )
    .await
    .expect("recv within 2s")
    .expect("recv ok");
    assert!(matches!(received, DaemonMessage::WidgetUpdate(_)));

    // Another ctl ping mid-stream should still work.
    let pong = ctl_round_trip(&socket_path, CtlRequest::Ping).await;
    assert!(matches!(pong, CtlResponse::Pong));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

// ---------------------------------------------------------------------------
// Telemetry integration (Phase 1.3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_module_publishes_initial_widget_update_over_ipc() {
    // MemoryModule publishes a memory WidgetUpdate during start() so this
    // test stays fast — no need to wait a tick interval. It also exercises
    // the /proc/meminfo read path on the actual host.
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 64,
            projects_dir: None,
        themes_dir: None,
    };
    let factory: ModuleFactory = Box::new(|_bus, publisher, _store, _projects| {
        vec![Box::new(MemoryModule::new(publisher)) as Box<dyn Module>]
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;

    let mut shell_conn = connect_shell(&socket_path).await;

    let received: DaemonMessage = tokio::time::timeout(
        Duration::from_secs(2),
        shell_conn.reader().recv(),
    )
    .await
    .expect("recv timeout")
    .expect("recv ok");

    match received {
        DaemonMessage::WidgetUpdate(update) => {
            assert_eq!(update.widget_id, "memory");
            assert_eq!(update.widget_type, "memory");
            assert_eq!(update.status, WidgetStatus::Normal);
            let used_percent = update
                .state
                .get("used_percent")
                .and_then(|v| v.as_f64())
                .expect("used_percent field present");
            assert!(
                (0.0..=100.0).contains(&used_percent),
                "used_percent out of range: {used_percent}"
            );
            let total_kb = update
                .state
                .get("total_kb")
                .and_then(|v| v.as_u64())
                .expect("total_kb field present");
            assert!(total_kb > 0);
        }
        other => panic!("expected memory WidgetUpdate, got {other:?}"),
    }

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

// ---------------------------------------------------------------------------
// Context engine integration (Phase 1.2 Step D)
// ---------------------------------------------------------------------------

/// Drain messages from the shell until we find one that matches `pred`, or
/// the timeout elapses. Returns all messages read along the way for
/// debugging.
async fn drain_until<F>(
    conn: &mut IpcConnection<JsonCodec>,
    mut pred: F,
    timeout: Duration,
) -> (DaemonMessage, Vec<DaemonMessage>)
where
    F: FnMut(&DaemonMessage) -> bool,
{
    let mut history = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "drain_until: timed out; observed {} messages: {:#?}",
                history.len(),
                history
            );
        }
        let msg = tokio::time::timeout(remaining, conn.reader().recv())
            .await
            .unwrap_or_else(|_| panic!("drain_until timeout; history: {history:#?}"))
            .expect("recv ok");
        if pred(&msg) {
            return (msg, history);
        }
        history.push(msg);
    }
}

#[tokio::test]
async fn context_engine_publishes_initial_bar_layout_on_shell_connect() {
    // Boot the daemon with a factory that only includes the context engine
    // (no Sway, which isn't available in CI).
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 64,
            projects_dir: None,
        themes_dir: None,
    };
    let factory: ModuleFactory = Box::new(|_bus, publisher, _store, _projects| {
        vec![Box::new(default_context_engine(publisher)) as Box<dyn Module>]
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;

    let mut shell_conn = connect_shell(&socket_path).await;

    // The context engine publishes an initial BarLayout during start().
    // We should see it before any event has fired.
    let (msg, _history) = drain_until(
        &mut shell_conn,
        |m| matches!(m, DaemonMessage::BarLayout(_)),
        Duration::from_secs(2),
    )
    .await;
    let layout = match msg {
        DaemonMessage::BarLayout(l) => l,
        other => panic!("expected BarLayout, got {other:?}"),
    };

    // Built-in widgets include clock (center) and battery/cpu/notifications
    // (right). Workspace-indicator goes left.
    assert!(layout.left.iter().any(|id| id == "workspace-indicator"));
    assert!(layout.center.iter().any(|id| id == "clock"));
    assert!(layout.right.iter().any(|id| id == "battery"));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

#[tokio::test]
async fn ctl_density_change_triggers_context_engine_republish() {
    // Boot with a context engine configured so that `bar.density ==
    // "compact"` promotes `notifications` to Compact. That rule lets us
    // observe the reactive path: ctl → bus → context-engine → IPC shell.
    use levshell_context::{parse_expression, CompiledRule};
    use levshell_ipc::Prominence;
    use levshell_modules::ContextEngineModule;

    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 64,
            projects_dir: None,
        themes_dir: None,
    };
    let factory: ModuleFactory = Box::new(|_bus, publisher, _store, _projects| {
        let rule = CompiledRule::new(
            "notifications",
            parse_expression(r#"bar.density == "compact""#).unwrap(),
            Prominence::Compact,
        );
        let engine = ContextEngineModule::new(publisher)
            .with_widgets(default_widgets())
            .with_rules(vec![rule]);
        vec![Box::new(engine) as Box<dyn Module>]
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;

    let mut shell_conn = connect_shell(&socket_path).await;

    // Drain the initial BarLayout burst so it doesn't confuse the match
    // below. notifications starts at IconOnly (its default).
    let _ = drain_until(
        &mut shell_conn,
        |m| {
            if let DaemonMessage::WidgetVisibility(v) = m {
                v.widget_id == "notifications" && v.prominence == Prominence::IconOnly
            } else {
                false
            }
        },
        Duration::from_secs(2),
    )
    .await;

    // Send the ctl density change. This publishes BarDensityRequested on
    // the bus, which the context engine subscribes to.
    let response = ctl_round_trip(
        &socket_path,
        CtlRequest::Density {
            mode: BarDensity::Compact,
        },
    )
    .await;
    assert!(matches!(response, CtlResponse::Ok));

    // Because rule evaluation promotes notifications from IconOnly to
    // Compact, the hysteresis activation_delay (default 2s) would normally
    // keep the committed value at IconOnly. But the ticker fires every
    // 500ms and observes the pending transition — so after ~2s we should
    // see a WidgetVisibility with prominence=Compact.
    let (msg, _history) = drain_until(
        &mut shell_conn,
        |m| {
            matches!(
                m,
                DaemonMessage::WidgetVisibility(v)
                    if v.widget_id == "notifications" && v.prominence == Prominence::Compact
            )
        },
        Duration::from_secs(5),
    )
    .await;
    let vis = match msg {
        DaemonMessage::WidgetVisibility(v) => v,
        _ => unreachable!(),
    };
    assert!(vis.visible);

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

// ---------------------------------------------------------------------------
// Palette integration (Phase 1.5)
// ---------------------------------------------------------------------------

/// Stub PaletteProvider used by the palette integration test. Always
/// returns a fixed set of items so assertions are deterministic without
/// depending on real apps / workspaces / notes.
struct StubPaletteProvider;

#[async_trait]
impl PaletteProvider for StubPaletteProvider {
    fn name(&self) -> &'static str {
        "stub"
    }
    async fn search(
        &self,
        _query: &str,
    ) -> Vec<levshell_modules::PaletteItem> {
        vec![
            levshell_modules::PaletteItem::new("stub", "one", "First item").with_score(0.9),
            levshell_modules::PaletteItem::new("stub", "two", "Second item").with_score(0.7),
        ]
    }
    async fn execute(
        &self,
        _item_id: &str,
    ) -> Result<(), levshell_modules::palette::provider::ProviderError> {
        Ok(())
    }
}

#[tokio::test]
async fn ctl_palette_open_publishes_widget_update_with_results() {
    // Full end-to-end verification of the Phase 1.5 palette path:
    // ctl sends `palette open` → daemon publishes PaletteActionRequested
    // → PaletteModule subscribes, refreshes from its providers, and
    // re-publishes a `command-palette` WidgetUpdate → we read it off
    // the shell socket and assert open=true with non-empty results.
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 64,
            projects_dir: None,
        themes_dir: None,
    };
    let factory: ModuleFactory = Box::new(|_bus, publisher, _store, _projects| {
        let palette = PaletteModule::new(publisher)
            .with_provider(Box::new(StubPaletteProvider));
        vec![Box::new(palette) as Box<dyn Module>]
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, factory, shutdown).await });
    wait_for_socket(&socket_path).await;

    let mut shell_conn = connect_shell(&socket_path).await;

    // First frame: initial closed-state palette WidgetUpdate from
    // PaletteModule::start().
    let (initial, _history) = drain_until(
        &mut shell_conn,
        |m| matches!(
            m,
            DaemonMessage::WidgetUpdate(u) if u.widget_id == "command-palette"
        ),
        Duration::from_secs(2),
    )
    .await;
    match initial {
        DaemonMessage::WidgetUpdate(u) => {
            let open = u.state.get("open").and_then(|v| v.as_bool());
            assert_eq!(open, Some(false), "initial palette should be closed");
        }
        _ => unreachable!(),
    }

    // Fire ctl palette open. This publishes PaletteActionRequested on
    // the bus, which the PaletteModule's subscription picks up and
    // turns into a fresh WidgetUpdate with open=true.
    let response = ctl_round_trip(
        &socket_path,
        CtlRequest::Palette {
            action: levshell_ipc::PaletteAction::Open,
            query: None,
        },
    )
    .await;
    assert!(matches!(response, CtlResponse::Ok));

    // Drain until we see the open-state palette update. There may be
    // other WidgetUpdates in-between (none expected in this minimal
    // factory, but be defensive).
    let (open_msg, _) = drain_until(
        &mut shell_conn,
        |m| matches!(
            m,
            DaemonMessage::WidgetUpdate(u)
                if u.widget_id == "command-palette"
                && u.state.get("open").and_then(|v| v.as_bool()) == Some(true)
        ),
        Duration::from_secs(3),
    )
    .await;
    let state = match open_msg {
        DaemonMessage::WidgetUpdate(u) => u.state,
        _ => unreachable!(),
    };
    let results = state
        .get("results")
        .and_then(|v| v.as_array())
        .expect("results array present");
    assert_eq!(results.len(), 2, "stub provider should have yielded two items");
    assert_eq!(
        results[0].get("title").and_then(|v| v.as_str()),
        Some("First item")
    );

    // Now simulate a shell-initiated close via ShellMessage.
    shell_conn
        .writer()
        .send(&ShellMessage::CommandPaletteClose)
        .await
        .unwrap();
    let (_closed, _) = drain_until(
        &mut shell_conn,
        |m| matches!(
            m,
            DaemonMessage::WidgetUpdate(u)
                if u.widget_id == "command-palette"
                && u.state.get("open").and_then(|v| v.as_bool()) == Some(false)
        ),
        Duration::from_secs(3),
    )
    .await;

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

// ---------------------------------------------------------------------------
// Sync adapter wiring (phase 2.3)
// ---------------------------------------------------------------------------

/// End-to-end: boot the daemon with an Obsidian adapter pointed at a
/// tempdir vault. After the first sync tick, the daemon's own data store
/// should contain notes corresponding to the vault files. Proves the
/// adapter runs under real daemon lifecycle (startup → sync engine spawn
/// → shutdown drains in-flight syncs).
#[tokio::test]
async fn daemon_runs_obsidian_adapter_against_tempdir_vault() {
    let (_dir, db_path, socket_path) = temp_paths();

    // Build a tempdir vault with two known files. The adapter will ingest
    // them on its first tick.
    let vault_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        vault_dir.path().join("one.md"),
        "---\ntitle: First Note\ntags: rust, shell\n---\nBody of one.",
    )
    .unwrap();
    std::fs::write(
        vault_dir.path().join("two.md"),
        "# Plain\n\nNo frontmatter.",
    )
    .unwrap();

    let config = DaemonConfig {
        db_path: db_path.clone(),
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
            projects_dir: None,
        themes_dir: None,
    };
    let factory = empty_factory();

    // Poll every second so the first tick runs well within the test
    // timeout. The adapter's first sync actually fires immediately after
    // spawn — the poll_interval only gates subsequent ticks.
    let vault_path = vault_dir.path().to_path_buf();
    let sync_factory: SyncAdapterFactory = Box::new(move || {
        let cfg = ObsidianConfig {
            vault_path,
            enabled: true,
            poll_interval_secs: 1,
            exclude_dirs: vec![".obsidian".into()],
        };
        vec![std::sync::Arc::new(ObsidianAdapter::new(cfg)) as std::sync::Arc<dyn SyncAdapter>]
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });

    let handle = tokio::spawn(async move {
        run_with_sync(config, factory, Some(sync_factory), shutdown).await
    });

    wait_for_socket(&socket_path).await;

    // Poll the data store until the first sync lands (or fail the test).
    // Opening a second DataStore on the same WAL-mode SQLite file is fine
    // — WAL permits concurrent readers and writers across connections.
    let mut attempts = 0;
    let notes = loop {
        attempts += 1;
        let store = DataStore::open(&db_path).await.expect("open store");
        let notes = store
            .list_notes(ListNotes::default())
            .await
            .expect("list notes");
        drop(store);
        if notes.len() == 2 {
            break notes;
        }
        if attempts > 60 {
            panic!("obsidian adapter never ingested vault files (got {} notes)", notes.len());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let titles: std::collections::HashSet<_> = notes.iter().map(|n| n.title.as_str()).collect();
    assert!(titles.contains("First Note"), "frontmatter title should win");
    assert!(titles.contains("two"), "filename stem when no frontmatter");

    let _ = shutdown_tx.send(());
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("daemon to exit")
        .expect("daemon task to join");
    result.expect("daemon to return Ok");
}

/// Daemon boots and exits cleanly even when the sync factory returns no
/// adapters. Covers the common case of a user with no sync TOML files.
#[tokio::test]
async fn daemon_boots_with_empty_sync_factory() {
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
            projects_dir: None,
        themes_dir: None,
    };

    let sync_factory: SyncAdapterFactory = Box::new(Vec::new);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move {
        run_with_sync(config, empty_factory(), Some(sync_factory), shutdown).await
    });
    wait_for_socket(&socket_path).await;

    // Sanity-check that the daemon is responsive.
    let response = ctl_round_trip(&socket_path, CtlRequest::Ping).await;
    assert!(matches!(response, CtlResponse::Pong));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

// ---------------------------------------------------------------------------
// Project registry + ctl attach/detach (phase 2.4)
// ---------------------------------------------------------------------------

/// End-to-end: start the daemon with a projects directory on disk,
/// connect a ctl client, list projects, attach a note, detach it. Proves
/// the full ctl → project_registry → data_store chain works over the
/// real IPC socket.
#[tokio::test]
async fn ctl_attach_and_detach_note_via_project_registry() {
    use levshell_data::NewNote;

    let (_dir, db_path, socket_path) = temp_paths();

    // Prepare a projects directory with one TOML file.
    let projects_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        projects_dir.path().join("target.toml"),
        r#"name = "Target"
tags = ["demo"]
"#,
    )
    .unwrap();

    let config = DaemonConfig {
        db_path: db_path.clone(),
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
        projects_dir: Some(projects_dir.path().to_path_buf()),
        themes_dir: None,
    };

    // Pre-seed a note in the store so the ctl client has something to
    // attach.
    let store = DataStore::open(&db_path).await.unwrap();
    let note = store
        .insert_note(NewNote {
            title: "To be attached".into(),
            content: "body".into(),
            project_id: None,
        })
        .await
        .unwrap();
    drop(store);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, empty_factory(), shutdown).await });
    wait_for_socket(&socket_path).await;

    // Projects list should contain our seeded project.
    let resp = ctl_round_trip(&socket_path, CtlRequest::Projects).await;
    let projects = match resp {
        CtlResponse::Projects { projects: p } => p,
        other => panic!("expected Projects, got {other:?}"),
    };
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Target");
    assert_eq!(projects[0].tags, vec!["demo"]);
    let project_id = projects[0].id.clone();

    // Attach by name.
    let resp = ctl_round_trip(
        &socket_path,
        CtlRequest::Attach {
            entity_type: "note".into(),
            entity_id: note.id.to_string(),
            project: "Target".into(),
        },
    )
    .await;
    assert!(matches!(resp, CtlResponse::Ok));

    // Verify the attach landed in the store.
    let store = DataStore::open(&db_path).await.unwrap();
    let attached = store.get_note(note.id).await.unwrap().unwrap();
    assert!(attached.project_id.is_some());
    assert_eq!(attached.project_id.unwrap().to_string(), project_id);
    drop(store);

    // Detach.
    let resp = ctl_round_trip(
        &socket_path,
        CtlRequest::Detach {
            entity_type: "note".into(),
            entity_id: note.id.to_string(),
        },
    )
    .await;
    assert!(matches!(resp, CtlResponse::Ok));

    let store = DataStore::open(&db_path).await.unwrap();
    let detached = store.get_note(note.id).await.unwrap().unwrap();
    assert!(detached.project_id.is_none());
    drop(store);

    // Attach by UUID string (should also work).
    let resp = ctl_round_trip(
        &socket_path,
        CtlRequest::Attach {
            entity_type: "note".into(),
            entity_id: note.id.to_string(),
            project: project_id.clone(),
        },
    )
    .await;
    assert!(matches!(resp, CtlResponse::Ok));

    // Unknown project returns an Error response.
    let resp = ctl_round_trip(
        &socket_path,
        CtlRequest::Attach {
            entity_type: "note".into(),
            entity_id: note.id.to_string(),
            project: "nonexistent".into(),
        },
    )
    .await;
    assert!(matches!(resp, CtlResponse::Error { .. }));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}

/// When no projects_dir is configured, attach/detach/projects all return
/// a clean error explaining the situation.
#[tokio::test]
async fn ctl_attach_without_registry_returns_error() {
    let (_dir, db_path, socket_path) = temp_paths();
    let config = DaemonConfig {
        db_path,
        socket_path: socket_path.clone(),
        publisher_capacity: 16,
        projects_dir: None,
        themes_dir: None,
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = shutdown_rx.await;
        });
    let handle = tokio::spawn(async move { run(config, empty_factory(), shutdown).await });
    wait_for_socket(&socket_path).await;

    let resp = ctl_round_trip(&socket_path, CtlRequest::Projects).await;
    let CtlResponse::Error { message } = resp else {
        panic!("expected Error, got {resp:?}");
    };
    assert!(message.contains("project registry not configured"));

    let resp = ctl_round_trip(
        &socket_path,
        CtlRequest::Attach {
            entity_type: "note".into(),
            entity_id: uuid::Uuid::now_v7().to_string(),
            project: "anything".into(),
        },
    )
    .await;
    assert!(matches!(resp, CtlResponse::Error { .. }));

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("daemon to exit");
}
