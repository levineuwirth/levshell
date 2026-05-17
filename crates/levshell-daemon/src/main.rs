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
    default_context_engine, default_palette_providers, default_warmup_state_path, AnkiDueModule,
    BatteryModule, ClockModule, CpuModule, FocusModeModule, GpuDashboardModule, HostRegistry,
    IdeationModule, InterruptionCostModule, MemoryModule, NetworkModule, NotificationsModule,
    PaletteModule, ProcessSniperModule, RemoteJobsModule, RemoteRunner, RubberDuckConfig,
    RubberDuckModule, SessionTimerConfig, SessionTimerModule, SshMonitorModule, SshRunner,
    SwayWorkspaceModule, UPowerWatcherModule, WarmupModule,
};
use levshell_sync::{
    AnkiConnectAdapter, AnkiConnectConfig, AnkiConnectConfigWatcher, CalDavAdapter, CalDavConfig,
    CalDavConfigWatcher, ObsidianAdapter, ObsidianConfig, ObsidianConfigWatcher, SyncAdapter,
    ZoteroAdapter, ZoteroConfig, ZoteroConfigWatcher,
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

    // Warmup module config (spec §2.12.1). Missing `warmup.toml` is
    // the common case — 4h gap, no calendar-day trigger.
    let warmup_config = levshell_config::default_config_base()
        .map(|dir| WarmupModule::load_config_from_dir(&dir))
        .unwrap_or_default();

    // Rubber-duck module config (spec §2.12.6). Missing
    // `rubber_duck.toml` is the common case — Ollama localhost +
    // llama3.2:3b. File can tune the model and endpoint.
    let rubber_duck_config = levshell_config::default_config_base()
        .and_then(|dir| {
            let path = dir.join("rubber_duck.toml");
            if !path.exists() {
                return None;
            }
            match RubberDuckConfig::load_from(&path) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to load rubber_duck.toml; using defaults"
                    );
                    None
                }
            }
        })
        .unwrap_or_default();
    tracing::info!(
        enabled = rubber_duck_config.enabled,
        endpoint = rubber_duck_config.endpoint,
        model = rubber_duck_config.model,
        "rubber-duck config loaded"
    );
    // Session timer config (spec §2.2.1). Missing `session_timer.toml`
    // → classic 25/5/15-after-4 Pomodoro.
    let session_timer_config = levshell_config::default_config_base()
        .map(|dir| SessionTimerConfig::load_from_dir(&dir))
        .unwrap_or_default();
    tracing::info!(
        work_minutes = session_timer_config.work_minutes,
        break_minutes = session_timer_config.break_minutes,
        "session-timer config loaded"
    );

    let warmup_state_path = default_warmup_state_path();
    tracing::info!(
        gap_secs = warmup_config.gap_secs,
        calendar_day_trigger = warmup_config.calendar_day_trigger,
        state = %warmup_state_path.display(),
        "warmup module config loaded"
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

    // Resolve the real layout pixel budget from sway once at startup
    // (monitor geometry rarely changes mid-session). Falls back to the
    // context engine's built-in default if sway is unreachable.
    let screen_width = levshell_modules::primary_output_width().await;
    match screen_width {
        Some(w) => tracing::info!(width = w, "context engine: real output width"),
        None => tracing::warn!("context engine: sway outputs unavailable; using default width"),
    }

    let factory: ModuleFactory = {
        let shared_profiles = shared_profiles.clone();
        let ideation_config = ideation_config.clone();
        let host_registry = host_registry.clone();
        let warmup_config = warmup_config.clone();
        let warmup_state_path = warmup_state_path.clone();
        let rubber_duck_config = rubber_duck_config.clone();
        let session_timer_config = session_timer_config.clone();
        Box::new(move |bus, publisher, store, projects| {
            let context_engine = {
                let ce = default_context_engine(publisher.clone())
                    .with_shared_profiles(shared_profiles.clone());
                match screen_width {
                    Some(w) => ce.with_available_width(w),
                    None => ce,
                }
            };
            let focus_mode = FocusModeModule::new(bus.clone(), shared_profiles);
            // Single registration point (M3.14) — the canonical built-in
            // provider set lives in levshell_modules::palette.
            let palette = PaletteModule::new(publisher.clone())
                .with_providers(default_palette_providers(store.clone()));
            let ideation = IdeationModule::with_config(
                bus.clone(),
                publisher.clone(),
                store.clone(),
                projects.clone(),
                ideation_config,
            );
            // Constructed before `store` is moved into warmup / before
            // `publisher` is moved into NetworkModule in the vec below.
            let clock = ClockModule::new(store.clone(), publisher.clone());
            let anki_due = AnkiDueModule::new(store.clone(), publisher.clone());
            let session_timer = SessionTimerModule::new(
                bus.clone(),
                publisher.clone(),
                session_timer_config.clone(),
            );
            let proc_sniper = ProcessSniperModule::new(publisher.clone());
            let warmup = WarmupModule::with_config(
                publisher.clone(),
                store,
                projects,
                warmup_config,
                warmup_state_path,
            );
            let rubber_duck =
                RubberDuckModule::with_config(publisher.clone(), rubber_duck_config);

            let notifications = NotificationsModule::with_notify_rust(publisher.clone());

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
                Box::new(InterruptionCostModule::new(publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(context_engine) as Box<dyn levshell_core::Module>,
                Box::new(focus_mode) as Box<dyn levshell_core::Module>,
                Box::new(CpuModule::new(bus.clone(), publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(MemoryModule::new(bus.clone(), publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(BatteryModule::new(bus.clone(), publisher.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(UPowerWatcherModule::new(bus.clone()))
                    as Box<dyn levshell_core::Module>,
                Box::new(NetworkModule::new(publisher)) as Box<dyn levshell_core::Module>,
                Box::new(palette) as Box<dyn levshell_core::Module>,
                Box::new(ideation) as Box<dyn levshell_core::Module>,
                Box::new(clock) as Box<dyn levshell_core::Module>,
                Box::new(anki_due) as Box<dyn levshell_core::Module>,
                Box::new(session_timer) as Box<dyn levshell_core::Module>,
                Box::new(proc_sniper) as Box<dyn levshell_core::Module>,
                Box::new(warmup) as Box<dyn levshell_core::Module>,
                Box::new(rubber_duck) as Box<dyn levshell_core::Module>,
                Box::new(notifications) as Box<dyn levshell_core::Module>,
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

    let caldav_adapter = sync_dir.as_deref().and_then(|dir| {
        let path = dir.join("caldav.toml");
        if !path.exists() {
            tracing::debug!(path = %path.display(), "no caldav.toml found");
            return None;
        }
        match CalDavConfig::load_from(&path) {
            Ok(cfg) => match CalDavAdapter::new(cfg.clone()) {
                Ok(a) => {
                    tracing::info!(
                        calendars = cfg.calendars.len(),
                        poll_secs = cfg.poll_interval_secs,
                        enabled = cfg.enabled,
                        "registering caldav sync adapter"
                    );
                    Some(Arc::new(a))
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to construct caldav adapter; skipping"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load caldav.toml; skipping adapter"
                );
                None
            }
        }
    });

    let _caldav_watcher = match (sync_dir.as_deref(), caldav_adapter.as_ref()) {
        (Some(dir), Some(adapter)) => match CalDavConfigWatcher::spawn(adapter.clone(), dir) {
            Ok(w) => {
                tracing::info!(
                    dir = %dir.display(),
                    "caldav config hot-reload watcher started"
                );
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to start caldav config watcher; hot-reload disabled"
                );
                None
            }
        },
        _ => None,
    };

    let sync_factory: SyncAdapterFactory = {
        let obsidian = obsidian_adapter.clone();
        let zotero = zotero_adapter.clone();
        let anki = ankiconnect_adapter.clone();
        let caldav = caldav_adapter.clone();
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
            if let Some(a) = caldav {
                adapters.push(a);
            }
            adapters
        })
    };

    // Shut down on either SIGINT (ctrl-c, interactive use) or SIGTERM
    // (sway killing its `exec` children on session exit, systemd stop).
    // Both must resolve the shutdown future so `run_with_sync` returns
    // and `IpcServer::drop` unlinks the socket — otherwise SIGTERM kills
    // the process before Drop runs and leaves a stale socket behind.
    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    r = tokio::signal::ctrl_c() => {
                        if let Err(e) = r {
                            tracing::error!(error = %e, "failed to install ctrl_c handler");
                        }
                        tracing::info!("SIGINT received; shutting down");
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("SIGTERM received; shutting down");
                    }
                }
            }
            Err(e) => {
                // SIGTERM handler unavailable — degrade to ctrl-c only
                // rather than refusing to start.
                tracing::error!(error = %e, "failed to install SIGTERM handler; ctrl-c only");
                if let Err(e) = tokio::signal::ctrl_c().await {
                    tracing::error!(error = %e, "failed to install ctrl_c handler");
                }
                tracing::info!("SIGINT received; shutting down");
            }
        }
    });

    run_with_sync(config, factory, Some(sync_factory), shutdown).await
}
