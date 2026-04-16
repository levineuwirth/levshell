//! Levshell daemon library — wires together the data store, event bus,
//! IPC server, and registered modules.
//!
//! `main.rs` is a thin shim that calls [`run`] with a default
//! [`DaemonConfig`] and a ctrl-c shutdown future. Tests construct their own
//! config (with tempfile-backed paths and a fake module factory) and call
//! the same entry point — there is no test-only flow.
//!
//! ## Lifecycle (Phase 1.1)
//!
//! 1. Initialize tracing.
//! 2. Open the [`DataStore`] at the configured path (creates parent dirs and
//!    runs migrations on first launch).
//! 3. Construct an [`EventBus`].
//! 4. Bind an [`IpcServer`] at the configured socket path.
//! 5. Build the [`SharedState`] and enter the accept loop. Each incoming
//!    connection sends a [`Hello`] handshake that tells the daemon whether it
//!    is the persistent QML shell or an ephemeral `levshell-ctl` client.
//! 6. On the first shell connection, spawn the writer/reader tasks, run the
//!    module factory, and register the modules on a fresh [`ModuleRunner`].
//!    Subsequent shell connections are rejected.
//! 7. Ctl connections spawn ephemeral handler tasks that read one
//!    [`CtlRequest`], act on it (publish to bus, inspect shared state),
//!    write one [`CtlResponse`], and close. Any number of ctl clients may
//!    connect concurrently, before or after the shell.
//! 8. Run until either the shutdown future fires, the writer task closes,
//!    or the reader task observes EOF.
//! 9. Drain the runner, drop the IPC server (which unlinks the socket file).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use levshell_core::{Event, EventBus, Module, ModuleRunner};
use levshell_data::DataStore;
use levshell_ipc::{
    default_socket_path, spawn_writer_task, BarDensity, ClientRole, CtlRequest, CtlResponse, Hello,
    IpcConnection, IpcServer, JsonCodec, PaletteAction, ProfileAction, ShellMessage,
    StatusSnapshot, WidgetPublisher, PROTOCOL_VERSION,
};
use levshell_sync::{SyncAdapter, SyncEngine, SyncEngineHandle};
use thiserror::Error;
use tokio::sync::oneshot;
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
/// is called once with a fresh `EventBus`, `WidgetPublisher`, and a clone
/// of the daemon's open `DataStore`, and returns the modules to register
/// on the runner. Returning an empty `Vec` is legal — the daemon still
/// runs the IPC pipeline.
///
/// The `DataStore` handle is cheap to clone (`Arc<Mutex<Connection>>`
/// internally); modules that don't need it can simply drop it.
pub type ModuleFactory =
    Box<dyn FnOnce(EventBus, WidgetPublisher, DataStore) -> Vec<Box<dyn Module>> + Send>;

/// Type alias for the sync-adapter factory closure passed to
/// [`run_with_sync`]. Called once at daemon startup (before any shell
/// connects); the returned adapters are registered on a fresh
/// [`SyncEngine`] which spawns one task per adapter. Returning an empty
/// vector leaves sync disabled — the rest of the daemon is unaffected.
pub type SyncAdapterFactory =
    Box<dyn FnOnce() -> Vec<std::sync::Arc<dyn SyncAdapter>> + Send>;

/// State shared between the accept loop and every ctl handler task. Cheap
/// to clone because all fields are either `Arc` or atomic.
#[derive(Clone)]
struct SharedState {
    bus: EventBus,
    socket_path: PathBuf,
    db_path: PathBuf,
    shell_connected: Arc<AtomicBool>,
    module_count: Arc<AtomicUsize>,
}

/// The daemon entry point. See the module-level docs for the lifecycle.
///
/// `shutdown` is awaited concurrently with the rest of the loop. Production
/// passes `tokio::signal::ctrl_c()`; tests pass a oneshot they trigger
/// manually.
///
/// This entry point does not register any sync adapters. Callers that want
/// external-tool integration (Obsidian, Zotero, …) use [`run_with_sync`]
/// and pass a [`SyncAdapterFactory`].
pub async fn run(
    config: DaemonConfig,
    factory: ModuleFactory,
    shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
) -> Result<()> {
    run_with_sync(config, factory, None, shutdown).await
}

/// Daemon entry point that additionally registers sync adapters via the
/// given [`SyncAdapterFactory`]. The sync engine starts before the IPC
/// server and runs independently of shell connection state — syncs happen
/// even if no shell is attached.
pub async fn run_with_sync(
    config: DaemonConfig,
    factory: ModuleFactory,
    sync_factory: Option<SyncAdapterFactory>,
    shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
) -> Result<()> {
    tracing::info!(
        db = %config.db_path.display(),
        socket = %config.socket_path.display(),
        "levshell-daemon starting"
    );

    // 1. Open the data store. This creates parent dirs and runs migrations.
    // `DataStore` is Clone (it holds an `Arc<Mutex<Connection>>`), so we
    // keep the owning handle here and pass clones to the module factory.
    let store = DataStore::open(&config.db_path)
        .await
        .with_context(|| format!("opening data store at {}", config.db_path.display()))?;
    tracing::info!("data store ready");

    // 2. Build the event bus.
    let bus = EventBus::new();

    // 2b. Start the sync engine before binding the IPC server. Sync runs
    // independently of shell connection state — the shell may come and go
    // but the daemon keeps pulling from external sources.
    let sync_handle = start_sync_engine(&store, &bus, sync_factory);

    // 3. Bind the IPC server.
    let server = IpcServer::bind(&config.socket_path)
        .with_context(|| format!("binding ipc socket at {}", config.socket_path.display()))?;
    tracing::info!(
        path = %config.socket_path.display(),
        "ipc server bound; accepting connections"
    );

    let state = SharedState {
        bus: bus.clone(),
        socket_path: config.socket_path.clone(),
        db_path: config.db_path.clone(),
        shell_connected: Arc::new(AtomicBool::new(false)),
        module_count: Arc::new(AtomicUsize::new(0)),
    };

    // The factory is a FnOnce, but the accept loop runs many times. Wrap it
    // in an Option so the first shell connection can `take()` it — on any
    // subsequent shell attempt the Option will already be None, which is
    // one of the signals we use to reject duplicate shells.
    let mut factory_slot: Option<ModuleFactory> = Some(factory);

    // Shutdown signal that fires when the shell disconnects or a fatal
    // error occurs inside a connection handler. Paired with the caller's
    // `shutdown` future via tokio::select! below.
    let (internal_shutdown_tx, mut internal_shutdown_rx) = oneshot::channel::<&'static str>();
    let mut internal_shutdown_tx = Some(internal_shutdown_tx);

    // Holds everything that must outlive the accept loop's first shell
    // connection: the module runner, the writer task's join handle, and
    // the reader task's join handle. Populated the first time a shell
    // connects.
    let mut runner: Option<ModuleRunner> = None;
    let mut writer_handle: Option<JoinHandle<()>> = None;
    let mut reader_handle: Option<JoinHandle<()>> = None;

    let mut shutdown = shutdown;

    // Main loop: accept connections forever, handshake, dispatch.
    //
    // Shell connections install the module runner and the writer/reader
    // tasks on first arrival. Ctl connections spawn ephemeral handlers.
    loop {
        tokio::select! {
            // Caller-provided shutdown (ctrl-c in production).
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received");
                break;
            }
            // Internal shutdown (shell disconnected, fatal error).
            reason = &mut internal_shutdown_rx => {
                match reason {
                    Ok(why) => tracing::info!(reason = %why, "internal shutdown"),
                    Err(_) => tracing::debug!("internal shutdown sender dropped"),
                }
                break;
            }
            // A new connection arrives.
            conn_result = server.accept() => {
                let conn = match conn_result {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = %e, "ipc accept failed; exiting");
                        break;
                    }
                };

                // Read exactly one Hello frame to decide the role.
                let (mut reader, writer) = conn.split();
                let hello: Hello = match reader.recv().await {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(error = %e, "rejecting connection: bad handshake");
                        continue;
                    }
                };
                let (role, protocol_version) = match hello {
                    Hello::Hello { role, protocol_version } => (role, protocol_version),
                    _ => {
                        tracing::warn!("unknown handshake variant; rejecting");
                        continue;
                    }
                };
                if protocol_version != PROTOCOL_VERSION {
                    tracing::warn!(
                        got = protocol_version,
                        want = PROTOCOL_VERSION,
                        "rejecting connection: protocol version mismatch"
                    );
                    // Best-effort error to the client; ignore failure.
                    let mut writer = writer;
                    let _ = writer
                        .send(&CtlResponse::Error {
                            message: format!(
                                "protocol version mismatch: daemon={PROTOCOL_VERSION}, client={protocol_version}"
                            ),
                        })
                        .await;
                    continue;
                }

                match role {
                    ClientRole::Shell => {
                        let Some(factory) = factory_slot.take() else {
                            tracing::warn!("rejecting shell connection: another shell has already attached this session");
                            let mut writer = writer;
                            let _ = writer
                                .send(&CtlResponse::Error {
                                    message: "another shell is already connected".into(),
                                })
                                .await;
                            continue;
                        };
                        state.shell_connected.store(true, Ordering::SeqCst);
                        tracing::info!("shell connected");

                        // Spawn writer + reader tasks and build modules.
                        let writer_task = spawn_writer_task(writer, config.publisher_capacity);
                        let shell_reader = spawn_shell_reader_task(
                            reader,
                            bus.clone(),
                            state.shell_connected.clone(),
                            internal_shutdown_tx.take(),
                        );

                        let modules =
                            factory(bus.clone(), writer_task.publisher.clone(), store.clone());
                        let mut r = ModuleRunner::new(bus.clone());
                        for module in modules {
                            r.register(module).await;
                        }
                        state.module_count.store(r.handles().len(), Ordering::SeqCst);

                        // Drop our publisher clone so the only remaining
                        // references live inside the registered modules.
                        drop(writer_task.publisher);

                        runner = Some(r);
                        writer_handle = Some(writer_task.handle);
                        reader_handle = Some(shell_reader);

                        // The writer task's `closed` oneshot fires when the
                        // peer hangs up or writes fail. Forward that into
                        // our internal shutdown so we can exit the accept
                        // loop cleanly.
                        //
                        // (The reader task already has its own path into
                        // internal_shutdown via the shutdown_tx we moved
                        // into it above.)
                        //
                        // NOTE: we don't hold a second internal_shutdown_tx
                        // for the writer-closed pathway; instead the reader
                        // task observes EOF slightly after the writer task
                        // does, and that's the branch we trust.
                        drop(writer_task.closed);
                    }
                    ClientRole::Ctl => {
                        tracing::debug!("ctl client connected");
                        tokio::spawn(handle_ctl_connection(
                            reader,
                            writer,
                            state.clone(),
                        ));
                    }
                }
            }
        }
    }

    // Cleanup.
    if let Some(r) = runner.take() {
        r.shutdown().await;
    }
    if let Some(h) = sync_handle {
        tracing::info!("waiting for sync adapters to finish in-flight syncs");
        h.shutdown().await;
    }
    if let Some(h) = writer_handle.take() {
        h.abort();
        let _ = h.await;
    }
    if let Some(h) = reader_handle.take() {
        h.abort();
        let _ = h.await;
    }
    drop(server);

    tracing::info!("levshell-daemon stopped");
    Ok(())
}

/// Build a [`SyncEngine`] from the given factory and spawn it. Returns
/// `None` if no factory was provided or the factory returned no adapters.
fn start_sync_engine(
    store: &DataStore,
    bus: &EventBus,
    sync_factory: Option<SyncAdapterFactory>,
) -> Option<SyncEngineHandle> {
    let factory = sync_factory?;
    let adapters = factory();
    if adapters.is_empty() {
        tracing::info!("no sync adapters registered");
        return None;
    }
    let mut engine = SyncEngine::new(store.clone(), bus.clone());
    let count = adapters.len();
    for adapter in adapters {
        tracing::info!(provider = %adapter.name(), "registering sync adapter");
        engine.register(adapter);
    }
    let handle = engine.spawn();
    tracing::info!(count, "sync engine started");
    Some(handle)
}

/// Spawn the shell-side reader task: drains [`ShellMessage`]s from the QML
/// shell and translates each variant into a corresponding [`Event`] on the
/// bus so interested modules (context engine, palette, …) can react. On
/// EOF, clears the `shell_connected` flag and fires the internal shutdown
/// signal so the accept loop exits.
fn spawn_shell_reader_task<C, R>(
    mut reader: levshell_ipc::IpcReader<C, R>,
    bus: EventBus,
    shell_connected: Arc<AtomicBool>,
    shutdown_tx: Option<oneshot::Sender<&'static str>>,
) -> JoinHandle<()>
where
    C: levshell_ipc::Codec,
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match reader.recv::<ShellMessage>().await {
                Ok(msg) => {
                    route_shell_message(&bus, msg);
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
        shell_connected.store(false, Ordering::SeqCst);
        if let Some(tx) = shutdown_tx {
            let _ = tx.send("shell disconnected");
        }
    })
}

/// Translate one [`ShellMessage`] into the matching [`Event`] on the bus.
/// Factored out so the reader task stays readable and so unit tests can
/// verify routing directly.
fn route_shell_message(bus: &EventBus, msg: ShellMessage) {
    match msg {
        ShellMessage::CommandPaletteQuery(q) => {
            tracing::debug!(query = %q.query, "shell: palette query");
            bus.publish(Event::CommandPaletteQueryReceived { query: q.query });
        }
        ShellMessage::CommandPaletteSelect(s) => {
            tracing::debug!(provider = %s.provider, item = %s.item_id, "shell: palette select");
            bus.publish(Event::CommandPaletteSelectReceived {
                provider: s.provider,
                item_id: s.item_id,
            });
        }
        ShellMessage::CommandPaletteClose => {
            tracing::debug!("shell: palette close");
            bus.publish(Event::PaletteActionRequested {
                action: "close".into(),
                query: None,
            });
        }
        ShellMessage::DensityChange(d) => {
            let mode_str = match d.mode {
                BarDensity::Full => "full",
                BarDensity::Compact => "compact",
                BarDensity::Hidden => "hidden",
            };
            tracing::debug!(mode = mode_str, "shell: density change");
            bus.publish(Event::BarDensityRequested {
                mode: mode_str.to_owned(),
            });
        }
        ShellMessage::WidgetAction(a) => {
            // Not yet routed — widgets that want actions will pick this
            // up in a later phase.
            tracing::debug!(
                widget_id = %a.widget_id,
                action = %a.action,
                "shell: widget action (ignored)"
            );
        }
        // ShellMessage is `#[non_exhaustive]` so unknown future variants
        // land here as a soft-ignore instead of breaking the build.
        _ => {
            tracing::debug!("shell: ignoring unknown ShellMessage variant");
        }
    }
}

/// Handle one ctl connection: read a single [`CtlRequest`], act on it, and
/// write a single [`CtlResponse`] back before closing.
async fn handle_ctl_connection(
    mut reader: levshell_ipc::IpcReader<JsonCodec, tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut writer: levshell_ipc::IpcWriter<JsonCodec, tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>>,
    state: SharedState,
) {
    let request: CtlRequest = match reader.recv().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "ctl: failed to read request");
            return;
        }
    };

    let response = dispatch_ctl_request(request, &state);

    if let Err(e) = writer.send(&response).await {
        tracing::warn!(error = %e, "ctl: failed to send response");
    }
    // Connection drops here — writer and reader go out of scope and close.
}

/// Map a [`CtlRequest`] to its [`CtlResponse`], publishing bus events for
/// action requests. Separated out so tests can hit the dispatch logic
/// directly without a socket.
fn dispatch_ctl_request(request: CtlRequest, state: &SharedState) -> CtlResponse {
    match request {
        CtlRequest::Ping => CtlResponse::Pong,

        CtlRequest::Status => CtlResponse::Status(StatusSnapshot {
            protocol_version: PROTOCOL_VERSION,
            socket_path: state.socket_path.display().to_string(),
            db_path: state.db_path.display().to_string(),
            shell_connected: state.shell_connected.load(Ordering::SeqCst),
            module_count: state.module_count.load(Ordering::SeqCst),
        }),

        CtlRequest::Density { mode } => {
            let mode_str = match mode {
                BarDensity::Full => "full",
                BarDensity::Compact => "compact",
                BarDensity::Hidden => "hidden",
            };
            state.bus.publish(Event::BarDensityRequested {
                mode: mode_str.to_owned(),
            });
            CtlResponse::Ok
        }

        CtlRequest::Profile { action, name } => {
            let action_str = match action {
                ProfileAction::Activate => "activate",
                ProfileAction::Cycle => "cycle",
                ProfileAction::Query => "query",
            };
            state.bus.publish(Event::ProfileActionRequested {
                action: action_str.to_owned(),
                name,
            });
            CtlResponse::Ok
        }

        CtlRequest::Palette { action, query } => {
            let action_str = match action {
                PaletteAction::Open => "open",
                PaletteAction::Close => "close",
                PaletteAction::Toggle => "toggle",
                PaletteAction::Query => "query",
            };
            state.bus.publish(Event::PaletteActionRequested {
                action: action_str.to_owned(),
                query,
            });
            CtlResponse::Ok
        }

        // `CtlRequest` is `#[non_exhaustive]`, so future variants land here
        // as a soft rejection instead of breaking the build.
        _ => CtlResponse::Error {
            message: "unsupported ctl request for this daemon version".into(),
        },
    }
}

// Re-export the server type used in IpcConnection so downstream tests don't
// need to depend directly on private imports.
#[doc(hidden)]
pub type _IpcConnection = IpcConnection<JsonCodec>;
