//! Integration tests for [`levshell_ipc`].
//!
//! Each test spins up an `IpcServer` on a tempfile-backed socket path,
//! connects a "shell" `UnixStream` from the test thread, and exercises one
//! slice of the protocol. We use `tokio::join!` to run accept and connect
//! concurrently so neither side has to know about the other's timing.

use std::path::PathBuf;
use std::time::Duration;

use levshell_ipc::{
    BarDensity, CommandPaletteQuery, DaemonMessage, DensityChange, IpcConnection, IpcError,
    IpcServer, JsonCodec, Prominence, ShellMessage, WidgetAction, WidgetStatus, WidgetUpdate,
    WidgetVisibility,
};
use serde_json::json;
use tokio::net::UnixStream;

fn temp_socket_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("levshell.sock");
    (dir, path)
}

async fn paired() -> (
    tempfile::TempDir,
    IpcServer,
    IpcConnection<JsonCodec>,
    IpcConnection<JsonCodec>,
) {
    let (dir, path) = temp_socket_path();
    let server = IpcServer::bind(&path).expect("bind ipc server");

    let accept_fut = server.accept();
    let connect_fut = UnixStream::connect(&path);
    let (server_conn, shell_stream) = tokio::join!(accept_fut, connect_fut);
    let server_conn = server_conn.expect("accept");
    let shell_stream = shell_stream.expect("client connect");
    let shell_conn = IpcConnection::<JsonCodec>::from_unix_stream(shell_stream);

    (dir, server, server_conn, shell_conn)
}

#[tokio::test]
async fn binds_and_unlinks_socket_on_drop() {
    let (_dir, path) = temp_socket_path();
    {
        let _server = IpcServer::bind(&path).unwrap();
        assert!(path.exists(), "socket file should exist while server is alive");
    }
    assert!(!path.exists(), "socket file should be unlinked on drop");
}

#[tokio::test]
async fn rebinding_clears_stale_socket_file() {
    let (_dir, path) = temp_socket_path();
    {
        let _s1 = IpcServer::bind(&path).unwrap();
    }
    // Manually re-create the file to simulate a stale socket from a crashed
    // previous run. (Drop would normally clean it up.)
    std::fs::write(&path, b"").unwrap();
    assert!(path.exists());

    let _s2 = IpcServer::bind(&path).expect("bind should clear stale file");
}

#[tokio::test]
async fn round_trip_widget_update_daemon_to_shell() {
    let (_dir, _server, mut server_conn, mut shell_conn) = paired().await;

    let msg = DaemonMessage::WidgetUpdate(WidgetUpdate {
        widget_id: "workspace-indicator".into(),
        widget_type: "workspace_indicator".into(),
        state: json!({ "active": "research", "all": ["research", "writing"] }),
        status: WidgetStatus::Normal,
    });

    server_conn.writer().send(&msg).await.unwrap();
    let received: DaemonMessage = shell_conn.reader().recv().await.unwrap();
    assert_eq!(received, msg);
}

#[tokio::test]
async fn round_trip_widget_action_shell_to_daemon() {
    let (_dir, _server, mut server_conn, mut shell_conn) = paired().await;

    let msg = ShellMessage::WidgetAction(WidgetAction {
        widget_id: "ssh-dashboard".into(),
        action: "reconnect".into(),
        data: json!({ "host": "gpu-cluster-3" }),
    });

    shell_conn.writer().send(&msg).await.unwrap();
    let received: ShellMessage = server_conn.reader().recv().await.unwrap();
    assert_eq!(received, msg);
}

#[tokio::test]
async fn multiple_messages_preserve_framing() {
    let (_dir, _server, mut server_conn, mut shell_conn) = paired().await;

    let msgs = vec![
        DaemonMessage::WidgetUpdate(WidgetUpdate {
            widget_id: "ws".into(),
            widget_type: "workspace_indicator".into(),
            state: json!(1),
            status: WidgetStatus::Normal,
        }),
        DaemonMessage::WidgetVisibility(WidgetVisibility {
            widget_id: "ws".into(),
            visible: true,
            prominence: Prominence::Compact,
        }),
        DaemonMessage::WidgetUpdate(WidgetUpdate {
            widget_id: "ws".into(),
            widget_type: "workspace_indicator".into(),
            state: json!("after"),
            status: WidgetStatus::Stale,
        }),
    ];

    for m in &msgs {
        server_conn.writer().send(m).await.unwrap();
    }
    for expected in &msgs {
        let got: DaemonMessage = shell_conn.reader().recv().await.unwrap();
        assert_eq!(&got, expected);
    }
}

#[tokio::test]
async fn shell_density_change_round_trip() {
    let (_dir, _server, mut server_conn, mut shell_conn) = paired().await;
    let msg = ShellMessage::DensityChange(DensityChange {
        mode: BarDensity::Compact,
    });
    shell_conn.writer().send(&msg).await.unwrap();
    let got: ShellMessage = server_conn.reader().recv().await.unwrap();
    assert_eq!(got, msg);
}

#[tokio::test]
async fn command_palette_query_round_trip() {
    let (_dir, _server, mut server_conn, mut shell_conn) = paired().await;
    let msg = ShellMessage::CommandPaletteQuery(CommandPaletteQuery {
        query: "zotero attention".into(),
    });
    shell_conn.writer().send(&msg).await.unwrap();
    let got: ShellMessage = server_conn.reader().recv().await.unwrap();
    assert_eq!(got, msg);
}

#[tokio::test]
async fn closed_peer_returns_connection_closed() {
    let (_dir, _server, mut server_conn, shell_conn) = paired().await;

    // Drop the shell side without sending anything.
    drop(shell_conn);

    // Give the kernel a moment to propagate the close.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let result: Result<ShellMessage, _> = server_conn.reader().recv().await;
    match result {
        Err(IpcError::ConnectionClosed) => {}
        other => panic!("expected ConnectionClosed, got {other:?}"),
    }
}

#[tokio::test]
async fn json_wire_format_uses_type_discriminator() {
    use levshell_ipc::Codec;
    let codec = JsonCodec;
    let msg = DaemonMessage::WidgetUpdate(WidgetUpdate {
        widget_id: "ws".into(),
        widget_type: "workspace_indicator".into(),
        state: json!({ "name": "research" }),
        status: WidgetStatus::Normal,
    });
    let bytes = codec.encode(&msg).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(value["type"], "widget_update");
    assert_eq!(value["widget_id"], "ws");
    assert_eq!(value["status"], "normal");

    let round_tripped: DaemonMessage = codec.decode(&bytes).unwrap();
    assert_eq!(round_tripped, msg);
}
