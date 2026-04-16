//! Levshell daemon entry point — a thin shim around
//! [`levshell_daemon::run_with_sync`].
//!
//! All meaningful logic lives in the library crate so the integration tests
//! can drive the same code path.

use std::pin::Pin;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use levshell_config::{load_profiles_from_dir, spawn_profile_watcher};
use levshell_daemon::{init_tracing, run_with_sync, DaemonConfig, ModuleFactory, SyncAdapterFactory};
use levshell_modules::{
    default_context_engine, AppLauncherProvider, BatteryModule, CpuModule, GpuDashboardModule,
    HostRegistry, IdeationModule, MemoryModule, NetworkModule, NoteSearchProvider, PaletteModule,
    PaletteProvider, RemoteJobsModule, RemoteRunner, SshMonitorModule, SshRunner,
    SwayWorkspaceModule, WorkspaceSwitcherProvider,
};
use levshell_sync::{
    AnkiConnectAdapter, AnkiConnectConfig, AnkiConnectConfigWatcher, ObsidianAdapter,
    ObsidianConfig, ObsidianConfigWatcher, SyncAdapter, ZoteroAdapter, ZoteroConfig,
    ZoteroConfigWatcher,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = DaemonConfig::with_defaults()?;

    // Load profiles from the user's config directory and wrap them in
    // a shared lock so the context engine and the hot-reload watcher
    // observe the same vec. Failures are logged and the daemon boots
    // with whatever parsed cleanly (possibly empty).
    let profiles_dir = levshell_config::default_profiles_dir();
    let initial_profiles = match profiles_dir.as_deref() {
        Some(dir) => {
            tracing::info!(dir = %dir.display(), "loading profiles");
            load_profiles_from_dir(dir)
        }
        None => {
            tracing::warn!("no XDG_CONFIG_HOME or HOME set; skipping profile load");
            Vec::new()
        }
    };
    tracing::info!(count = initial_profiles.len(), "profiles loaded");
    let shared_profiles = Arc::new(RwLock::new(initial_profiles));

    // Spawn the profile hot-reload watcher when a profiles dir is
    // configured. Kept alive by this binding until main returns.
    let _profile_watcher = match profiles_dir.as_deref() {
        Some(dir) => match spawn_profile_watcher(dir, shared_profiles.clone()) {
            Ok(w) => {
                tracing::info!(
                    dir = %dir.display(),
                    "profile hot-reload watcher started"
                );
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to start profile watcher; hot-reload disabled"
                );
                None
            }
        },
        None => None,
    };

    // Load the ideation engine's config up front so the factory
    // closure stays sync and the load failure path is logged once,
    // not per shell connection. Missing `ideation.toml` is normal —
    // the engine starts with defaults.
    let ideation_config = levshell_config::default_config_base()
        .map(|dir| IdeationModule::load_config_from_dir(&dir))
        .unwrap_or_default();
    tracing::info!(
        lambda_min = ideation_config.lambda_minutes,
        tick_secs = ideation_config.tick_secs,
        enabled = ideation_config.enabled,
        "ideation engine config loaded"
    );

    // Host registry for the SSH / GPU / remote-jobs triad. Read once
    // at boot — a future phase will add inotify hot-reload matching
    // profiles/projects. Missing directory is normal (no remote
    // hosts configured).
    let host_registry = match levshell_config::default_config_base()
        .map(|b| b.join("hosts"))
    {
        Some(dir) => match HostRegistry::load_from_dir(&dir) {
            Ok(r) => {
                tracing::info!(
                    dir = %dir.display(),
                    host_count = r.hosts().len(),
                    "host registry loaded"
                );
                Arc::new(r)
            }
            Err(e) => {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "failed to load host registry; remote modules dormant"
                );
                Arc::new(HostRegistry::default())
            }
        },
        None => {
            tracing::debug!("no config base; remote modules dormant");
            Arc::new(HostRegistry::default())
        }
    };

    let factory: ModuleFactory = {
        let shared_profiles = shared_profiles.clone();
        let ideation_config = ideation_config.clone();
        let host_registry = host_registry.clone();
        Box::new(move |bus, publisher, store, projects| {
            let context_engine = default_context_engine(publisher.clone())
                .with_shared_profiles(shared_profiles);
            let palette_providers: Vec<Box<dyn PaletteProvider>> = vec![
                Box::new(AppLauncherProvider::new()),
                Box::new(WorkspaceSwitcherProvider::new()),
                Box::new(NoteSearchProvider::new(store.clone())),
            ];
            let palette =
                PaletteModule::new(publisher.clone()).with_providers(palette_providers);
            let ideation = IdeationModule::with_config(
                bus.clone(),
                publisher.clone(),
                store,
                projects,
                ideation_config,
            );

            // Single shared SshRunner across the three remote modules
            // so ControlMaster TCP connections are reused for every
            // probe regardless of which module issued it.
            let ssh_runner: Arc<dyn RemoteRunner> = Arc::new(SshRunner::new());
            let ssh_monitor = SshMonitorModule::new(
                host_registry.clone(),
                ssh_runner.clone(),
                publisher.clone(),
                bus.clone(),
            );
            let gpu_dashboard = GpuDashboardModule::new(
                host_registry.clone(),
                ssh_runner.clone(),
                publisher.clone(),
            );
            let remote_jobs =
                RemoteJobsModule::new(host_registry.clone(), ssh_runner, publisher.clone());

            vec![
                Box::new(SwayWorkspaceModule::new(bus.clone(), publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(context_engine) as Box<dyn levshell_core::Module>,
                Box::new(CpuModule::new(publisher.clone())) as Box<dyn levshell_core::Module>,
                Box::new(MemoryModule::new(publisher.clone())) as Box<dyn levshell_core::Module>,
                Box::new(BatteryModule::new(bus.clone(), publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(NetworkModule::new(publisher)) as Box<dyn levshell_core::Module>,
                Box::new(palette) as Box<dyn levshell_core::Module>,
                Box::new(ideation) as Box<dyn levshell_core::Module>,
                Box::new(ssh_monitor) as Box<dyn levshell_core::Module>,
                Box::new(gpu_dashboard) as Box<dyn levshell_core::Module>,
                Box::new(remote_jobs) as Box<dyn levshell_core::Module>,
            ]
        })
    };

    // Build the sync adapter(s) and the matching config watcher(s).
    // We construct adapters *before* calling run_with_sync so that we
    // can hold concrete-typed `Arc<ObsidianAdapter>` handles for the
    // hot-reload watchers (spec §3.9) alongside the trait-object
    // versions the sync engine wants.
    let sync_dir = levshell_config::default_sync_dir();
    if sync_dir.is_none() {
        tracing::warn!("no XDG_CONFIG_HOME or HOME set; skipping sync adapter load");
    }

    let obsidian_adapter = sync_dir.as_deref().and_then(|dir| {
        let path = dir.join("obsidian.toml");
        if !path.exists() {
            tracing::debug!(path = %path.display(), "no obsidian.toml found");
            return None;
        }
        match ObsidianConfig::load_from(&path) {
            Ok(cfg) => {
                tracing::info!(
                    vault = %cfg.vault_path.display(),
                    poll_secs = cfg.poll_interval_secs,
                    enabled = cfg.enabled,
                    "registering obsidian sync adapter"
                );
                Some(Arc::new(ObsidianAdapter::new(cfg)))
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load obsidian.toml; skipping adapter"
                );
                None
            }
        }
    });

    // Spawn the Obsidian config watcher when we have both a sync_dir and
    // a live adapter. The watcher is kept alive by this binding — when
    // run_with_sync returns and main drops it, the watcher stops.
    let _obsidian_watcher = match (sync_dir.as_deref(), obsidian_adapter.as_ref()) {
        (Some(dir), Some(adapter)) => {
            match ObsidianConfigWatcher::spawn(adapter.clone(), dir) {
                Ok(w) => {
                    tracing::info!(
                        dir = %dir.display(),
                        "obsidian config hot-reload watcher started"
                    );
                    Some(w)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to start obsidian config watcher; hot-reload disabled"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let zotero_adapter = sync_dir.as_deref().and_then(|dir| {
        let path = dir.join("zotero.toml");
        if !path.exists() {
            tracing::debug!(path = %path.display(), "no zotero.toml found");
            return None;
        }
        match ZoteroConfig::load_from(&path) {
            Ok(cfg) => {
                tracing::info!(
                    database = %cfg.database_path.display(),
                    poll_secs = cfg.poll_interval_secs,
                    enabled = cfg.enabled,
                    "registering zotero sync adapter"
                );
                Some(Arc::new(ZoteroAdapter::new(cfg)))
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load zotero.toml; skipping adapter"
                );
                None
            }
        }
    });

    let _zotero_watcher = match (sync_dir.as_deref(), zotero_adapter.as_ref()) {
        (Some(dir), Some(adapter)) => match ZoteroConfigWatcher::spawn(adapter.clone(), dir) {
            Ok(w) => {
                tracing::info!(
                    dir = %dir.display(),
                    "zotero config hot-reload watcher started"
                );
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to start zotero config watcher; hot-reload disabled"
                );
                None
            }
        },
        _ => None,
    };

    let ankiconnect_adapter = sync_dir.as_deref().and_then(|dir| {
        let path = dir.join("ankiconnect.toml");
        if !path.exists() {
            tracing::debug!(path = %path.display(), "no ankiconnect.toml found");
            return None;
        }
        match AnkiConnectConfig::load_from(&path) {
            Ok(cfg) => match AnkiConnectAdapter::new(cfg.clone()) {
                Ok(a) => {
                    tracing::info!(
                        endpoint = %cfg.endpoint,
                        poll_secs = cfg.poll_interval_secs,
                        enabled = cfg.enabled,
                        "registering ankiconnect sync adapter"
                    );
                    Some(Arc::new(a))
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to construct ankiconnect adapter; skipping"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load ankiconnect.toml; skipping adapter"
                );
                None
            }
        }
    });

    let _ankiconnect_watcher = match (sync_dir.as_deref(), ankiconnect_adapter.as_ref()) {
        (Some(dir), Some(adapter)) => {
            match AnkiConnectConfigWatcher::spawn(adapter.clone(), dir) {
                Ok(w) => {
                    tracing::info!(
                        dir = %dir.display(),
                        "ankiconnect config hot-reload watcher started"
                    );
                    Some(w)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to start ankiconnect config watcher; hot-reload disabled"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let sync_factory: SyncAdapterFactory = {
        let obsidian = obsidian_adapter.clone();
        let zotero = zotero_adapter.clone();
        let anki = ankiconnect_adapter.clone();
        Box::new(move || {
            let mut adapters: Vec<Arc<dyn SyncAdapter>> = Vec::new();
            if let Some(a) = obsidian {
                adapters.push(a);
            }
            if let Some(a) = zotero {
                adapters.push(a);
            }
            if let Some(a) = anki {
                adapters.push(a);
            }
            adapters
        })
    };

    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl_c handler");
        }
        tracing::info!("ctrl-c received");
    });

    run_with_sync(config, factory, Some(sync_factory), shutdown).await
}
