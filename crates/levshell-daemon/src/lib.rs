//! Levshell daemon library — wires together the data store, event bus,
//! IPC server, and registered modules.
//!
//! `main.rs` is a thin shim that calls [`run`] with a default
//! [`DaemonConfig`] and a ctrl-c shutdown future. Tests construct their own
//! config (with tempfile-backed paths and a fake module factory) and call
//! the same entry point — there is no test-only flow.
//!
//! ## Lifecycle
//!
//! 1. Initialize tracing.
//! 2. Open the [`DataStore`] at the configured path (creates parent dirs and
//!    runs migrations on first launch).
//! 3. Construct an [`EventBus`].
//! 4. Bind an [`IpcServer`] at the configured socket path.
//! 5. Wait for the QML shell to `accept` exactly once.
//! 6. Split the connection into reader / writer halves.
//! 7. Spawn the writer task and capture its [`WidgetPublisher`].
//! 8. Spawn the reader task that drains [`ShellMessage`]s from the shell.
//! 9. Build modules via the caller-supplied factory closure (which is the
//!    only place that knows how to construct modules with the publisher
//!    handle).
//! 10. Register and start each module on a fresh [`ModuleRunner`].
//! 11. Run until either the shutdown future fires, the writer task closes,
//!     or the reader task observes EOF.
//! 12. Drain the runner, drop the IPC server (which unlinks the socket file).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::pin::Pin;

use anyhow::{Context, Result};
use levshell_core::{EventBus, Module, ModuleRunner};
use levshell_data::DataStore;
use levshell_ipc::{
    default_socket_path, spawn_writer_task, IpcServer, ShellMessage, WidgetPublisher,
};
use thiserror::Error;
use tokio::task::JoinHandle;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("$HOME and $XDG_DATA_HOME are both unset; cannot derive default db path")]
    NoHome,
}

/// Configuration for [`run`]. All fields have sane defaults via
/// [`Self::with_defaults`] for the production binary; tests construct their
/// own values to point at tempfiles.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the SQLite database file. Parent directories are created.
    pub db_path: PathBuf,
    /// Path the IPC server binds at.
    pub socket_path: PathBuf,
    /// Channel capacity between modules and the IPC writer task.
    pub publisher_capacity: usize,
}

impl DaemonConfig {
    /// Defaults appropriate for the production binary:
    /// `$XDG_DATA_HOME/levshell/levshell.db` (or `~/.local/share/...`) and
    /// `$XDG_RUNTIME_DIR/levshell.sock`.
    pub fn with_defaults() -> Result<Self> {
        Ok(Self {
            db_path: default_db_path()?,
            socket_path: default_socket_path().context("resolving default socket path")?,
            publisher_capacity: 256,
        })
    }
}

fn default_db_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or(DaemonError::NoHome)?;
    Ok(base.join("levshell").join("levshell.db"))
}

/// Initialize the global tracing subscriber. Idempotent: if a subscriber is
/// already installed (e.g. by a test harness) this returns Ok without
/// touching the global state.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .try_init();
}

/// Type alias for the module factory closure passed to [`run`]. The closure
/// is called once with a fresh `EventBus` and `WidgetPublisher` and returns
/// the modules to register on the runner. Returning an empty `Vec` is
/// legal — the daemon still runs the IPC pipeline.
pub type ModuleFactory =
    Box<dyn FnOnce(EventBus, WidgetPublisher) -> Vec<Box<dyn Module>> + Send>;

/// The daemon entry point. See the module-level docs for the lifecycle.
///
/// `shutdown` is awaited concurrently with the rest of the loop. Production
/// passes `tokio::signal::ctrl_c()`; tests pass a oneshot they trigger
/// manually.
pub async fn run(
    config: DaemonConfig,
    factory: ModuleFactory,
    shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
) -> Result<()> {
    tracing::info!(
        db = %config.db_path.display(),
        socket = %config.socket_path.display(),
        "levshell-daemon starting"
    );

    // 1. Open the data store. This creates parent dirs and runs migrations.
    let _store = DataStore::open(&config.db_path)
        .await
        .with_context(|| format!("opening data store at {}", config.db_path.display()))?;
    tracing::info!("data store ready");

    // 2. Build the event bus.
    let bus = EventBus::new();

    // 3. Bind the IPC server.
    let server = IpcServer::bind(&config.socket_path)
        .with_context(|| format!("binding ipc socket at {}", config.socket_path.display()))?;
    tracing::info!(
        path = %config.socket_path.display(),
        "ipc server bound; waiting for shell to connect"
    );

    // 4. Wait for the (single) shell connection.
    let conn = server.accept().await.context("accepting shell connection")?;
    tracing::info!("shell connected");

    let (reader, writer) = conn.split();

    // 5. Spawn writer + reader tasks.
    let writer_task = spawn_writer_task(writer, config.publisher_capacity);
    let reader_handle = spawn_reader_task(reader);

    // 6. Build modules via the factory and register them.
    let modules = factory(bus.clone(), writer_task.publisher.clone());
    let mut runner = ModuleRunner::new(bus.clone());
    for module in modules {
        runner.register(module).await;
    }

    // Drop our publisher clone so the only remaining strong references live
    // inside the registered modules. When all modules drop them at shutdown
    // the writer task's mpsc closes naturally.
    drop(writer_task.publisher);

    // 7. Run until something tells us to stop.
    let mut shutdown = shutdown;
    let mut closed = writer_task.closed;
    let mut reader_handle = reader_handle;
    tokio::select! {
        _ = &mut shutdown => {
            tracing::info!("shutdown signal received");
        }
        _ = &mut closed => {
            tracing::info!("ipc writer closed; shutting down");
        }
        _ = &mut reader_handle => {
            tracing::info!("ipc reader exited; shutting down");
        }
    }

    // 8. Drain the runner, then everything else cleans itself up via Drop.
    runner.shutdown().await;
    reader_handle.abort();
    writer_task.handle.abort();
    let _ = reader_handle.await;
    let _ = writer_task.handle.await;
    drop(server);

    tracing::info!("levshell-daemon stopped");
    Ok(())
}

/// Spawn the reader-side task: drains [`ShellMessage`]s from the shell and
/// (for Phase 0) just logs them. Step 0.5 deliberately doesn't route shell
/// events into the bus or runner — that's Phase 1 work alongside the
/// command palette.
fn spawn_reader_task<C, R>(mut reader: levshell_ipc::IpcReader<C, R>) -> JoinHandle<()>
where
    C: levshell_ipc::Codec,
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match reader.recv::<ShellMessage>().await {
                Ok(msg) => {
                    tracing::info!(?msg, "ipc reader: received shell message");
                }
                Err(levshell_ipc::IpcError::ConnectionClosed) => {
                    tracing::info!("ipc reader: shell closed connection");
                    break;
                }
                Err(e) => {
                    tracing::error!(error = %e, "ipc reader: read error, exiting");
                    break;
                }
            }
        }
    })
}
