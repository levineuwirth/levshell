//! [`WidgetPublisher`] — the cheap-to-clone handle modules use to push
//! [`DaemonMessage`]s out to the QML shell.
//!
//! There is exactly one writer task per IPC connection. Modules never touch
//! the [`IpcWriter`] directly; instead they hold a `WidgetPublisher` (an
//! `mpsc::Sender<DaemonMessage>`) and the writer task drains the matching
//! receiver in a loop, encoding each message and writing one newline-delimited
//! frame to the socket. This avoids contention on the writer half and gives
//! the daemon a single place to react when the connection drops.

use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::codec::Codec;
use crate::error::IpcError;
use crate::messages::DaemonMessage;
use crate::server::IpcWriter;

/// Cheap-to-clone handle that modules use to publish [`DaemonMessage`]s
/// to the IPC writer task. Backed by a bounded `mpsc::Sender`; if the
/// channel is full, [`Self::try_send`] returns the message back to the
/// caller and [`Self::send`] awaits capacity.
#[derive(Clone, Debug)]
pub struct WidgetPublisher {
    tx: mpsc::Sender<DaemonMessage>,
}

impl WidgetPublisher {
    fn new(tx: mpsc::Sender<DaemonMessage>) -> Self {
        Self { tx }
    }

    /// Non-blocking send. Returns `Err` with the original message if the
    /// channel is full or the writer task has exited.
    pub fn try_send(
        &self,
        msg: DaemonMessage,
    ) -> std::result::Result<(), mpsc::error::TrySendError<DaemonMessage>> {
        self.tx.try_send(msg)
    }

    /// Async send. Awaits channel capacity. Returns `Err` if the writer
    /// task has exited.
    pub async fn send(
        &self,
        msg: DaemonMessage,
    ) -> std::result::Result<(), mpsc::error::SendError<DaemonMessage>> {
        self.tx.send(msg).await
    }

    /// `true` if the writer task has dropped its receiver.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Result of [`spawn_writer_task`].
pub struct WriterTask {
    /// Hand this to every module that needs to publish widget updates.
    pub publisher: WidgetPublisher,
    /// The spawned writer task. Awaiting it joins the loop after the
    /// publisher channel closes (or the writer hits an IO error).
    pub handle: JoinHandle<()>,
    /// Fires when the writer task exits because the IPC peer hung up
    /// or returned an unrecoverable IO error. The daemon awaits this
    /// in its top-level select! to know when to start shutting down.
    pub closed: oneshot::Receiver<()>,
}

/// Spawn a background task that owns the [`IpcWriter`], drains
/// [`DaemonMessage`]s from a bounded channel, and writes each as a
/// newline-delimited frame. Returns a [`WidgetPublisher`] handle, the task
/// join handle, and a oneshot that fires when the loop exits.
///
/// The default writer half is `BufWriter<OwnedWriteHalf>`, but the helper is
/// generic over the underlying writer so unit tests can use a duplex pipe
/// instead of a real socket.
pub fn spawn_writer_task<C, W>(mut writer: IpcWriter<C, W>, capacity: usize) -> WriterTask
where
    C: Codec,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<DaemonMessage>(capacity.max(1));
    let (closed_tx, closed_rx) = oneshot::channel();

    let handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = writer.send(&msg).await {
                match &e {
                    IpcError::ConnectionClosed => {
                        tracing::info!("ipc writer: peer closed connection");
                    }
                    _ => {
                        tracing::error!(error = %e, "ipc writer task failed");
                    }
                }
                break;
            }
        }
        // Notify the daemon that the writer is gone. The receiver may have
        // already been dropped if the daemon initiated shutdown — that's
        // fine, send returns Err and we ignore it.
        let _ = closed_tx.send(());
    });

    WriterTask {
        publisher: WidgetPublisher::new(tx),
        handle,
        closed: closed_rx,
    }
}

