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
use levshell_core::{EventBus, Event, EventKind, Module, ModuleResult};
use levshell_daemon::{run, DaemonConfig, ModuleFactory};
use levshell_ipc::{
    DaemonMessage, IpcConnection, JsonCodec, ShellMessage, WidgetAction, WidgetPublisher,
    WidgetStatus, WidgetUpdate,
};
use serde_json::json;
use tokio::net::UnixStream;
use tokio::sync::oneshot;

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
    };

    let factory: ModuleFactory = Box::new(|bus, publisher| {
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

    // Wait for the daemon to bind the socket.
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(socket_path.exists(), "daemon should have bound the socket");

    // Connect as the "shell".
    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("client connect");
    let mut shell_conn = IpcConnection::<JsonCodec>::from_unix_stream(stream);

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
