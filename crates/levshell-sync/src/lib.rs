//! Levshell sync engine.
//!
//! Hosts the [`SyncAdapter`] trait (spec §3.3.4) and the [`SyncEngine`]
//! scheduler that runs each adapter on its configured interval, publishing
//! lifecycle events on [`levshell_core::EventBus`]. Adapters are isolated
//! from the rest of the daemon: they read their external source, write
//! directly to [`levshell_data::DataStore`], and return a [`SyncReport`] —
//! a sync failure never propagates into the shell.
//!
//! # Layout
//!
//! - [`adapter`]: the `SyncAdapter` trait, `SyncContext`, `SyncReport`,
//!   `SyncStatus`, `SyncError`.
//! - [`engine`]: the `SyncEngine` scheduler and `SyncEngineHandle`.
//!
//! # Adding a new adapter
//!
//! 1. Create a new submodule in this crate (e.g. `obsidian`).
//! 2. Implement `SyncAdapter` — the trait enforces `Send + Sync + 'static`
//!    so the engine can hold `Arc<dyn SyncAdapter>`.
//! 3. Register the adapter with the engine at daemon startup:
//!    ```ignore
//!    let mut engine = SyncEngine::new(store, bus);
//!    engine.register(Arc::new(ObsidianAdapter::new(config)));
//!    let handle = engine.spawn();
//!    ```

#![forbid(unsafe_code)]

pub mod adapter;
pub mod ankiconnect;
pub mod caldav;
pub mod engine;
pub mod mlflow;
pub mod obsidian;
pub mod zotero;

pub use adapter::{
    Result, SyncAdapter, SyncConflict, SyncContext, SyncError, SyncReport, SyncStatus,
};
pub use ankiconnect::{
    AnkiClient, AnkiClientError, AnkiConnectAdapter, AnkiConnectConfig, AnkiConnectConfigError,
    AnkiConnectConfigWatcher, AnkiConnectHttpClient,
};
pub use caldav::{
    CalDavAdapter, CalDavClient, CalDavConfig, CalDavConfigError, CalDavConfigWatcher,
    CalDavError, CalDavHttpClient, CalendarSource, DavEntry,
};
pub use engine::{SyncEngine, SyncEngineConfig, SyncEngineHandle};
pub use mlflow::{MlflowAdapter, MlflowConfig, MlflowConfigError};
pub use obsidian::{
    ObsidianAdapter, ObsidianConfig, ObsidianConfigError, ObsidianConfigWatcher,
};
pub use zotero::{
    ZoteroAdapter, ZoteroConfig, ZoteroConfigError, ZoteroConfigWatcher,
};
