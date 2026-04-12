//! Tests for [`levshell_ipc::WidgetPublisher`] and [`spawn_writer_task`].

use std::path::PathBuf;
use std::time::Duration;

use levshell_ipc::{
    spawn_writer_task, DaemonMessage, IpcConnection, IpcServer, JsonCodec, WidgetStatus,
    WidgetUpdate,
};
use serde_json::json;
use tokio::net::UnixStream;

fn temp_socket_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("levshell.sock");
    (dir, path)
}

#[tokio::test]
async fn publisher_routes_messages_through_writer_task() {
    let (_dir, path) = temp_socket_path();
    let server = IpcServer::bind(&path).unwrap();

    let accept_fut = server.accept();
    let connect_fut = UnixStream::connect(&path);
    let (server_conn, shell_stream) = tokio::join!(accept_fut, connect_fut);
    let server_conn = server_conn.unwrap();
    let shell_stream = shell_stream.unwrap();

    let (_server_reader, server_writer) = server_conn.split();
    let mut shell_conn = IpcConnection::<JsonCodec>::from_unix_stream(shell_stream);

    let task = spawn_writer_task(server_writer, 16);

    let msg = DaemonMessage::WidgetUpdate(WidgetUpdate {
        widget_id: "workspace-indicator".into(),
        widget_type: "workspace_indicator".into(),
        state: json!({ "active": "research" }),
        status: WidgetStatus::Normal,
    });
    task.publisher.send(msg.clone()).await.unwrap();

    let received: DaemonMessage = shell_conn.reader().recv().await.unwrap();
    assert_eq!(received, msg);

    // Drop the publisher → writer task drains and exits → closed signal fires.
    drop(task.publisher);
    tokio::time::timeout(Duration::from_secs(1), task.handle)
        .await
        .expect("writer task to exit")
        .unwrap();
}

#[tokio::test]
async fn closed_signal_fires_when_peer_disappears() {
    let (_dir, path) = temp_socket_path();
    let server = IpcServer::bind(&path).unwrap();

    let accept_fut = server.accept();
    let connect_fut = UnixStream::connect(&path);
    let (server_conn, shell_stream) = tokio::join!(accept_fut, connect_fut);
    let server_conn = server_conn.unwrap();
    let shell_stream = shell_stream.unwrap();

    let (_server_reader, server_writer) = server_conn.split();
    let task = spawn_writer_task(server_writer, 16);

    // Drop the shell side without reading anything.
    drop(shell_stream);
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Push a message — the writer will detect the broken pipe and exit.
    let _ = task
        .publisher
        .send(DaemonMessage::WidgetUpdate(WidgetUpdate {
            widget_id: "x".into(),
            widget_type: "x".into(),
            state: serde_json::Value::Null,
            status: WidgetStatus::Normal,
        }))
        .await;

    // The closed signal should fire within a short window.
    tokio::time::timeout(Duration::from_secs(1), task.closed)
        .await
        .expect("closed signal within 1s")
        .expect("oneshot delivery");
}
