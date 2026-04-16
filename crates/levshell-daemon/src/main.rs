//! Levshell daemon entry point — a thin shim around
//! [`levshell_daemon::run_with_sync`].
//!
//! All meaningful logic lives in the library crate so the integration tests
//! can drive the same code path.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use levshell_config::load_profiles_from_dir;
use levshell_daemon::{init_tracing, run_with_sync, DaemonConfig, ModuleFactory, SyncAdapterFactory};
use levshell_modules::{
    default_context_engine, AppLauncherProvider, BatteryModule, CpuModule, MemoryModule,
    NetworkModule, NoteSearchProvider, PaletteModule, PaletteProvider, SwayWorkspaceModule,
    WorkspaceSwitcherProvider,
};
use levshell_sync::{ObsidianAdapter, ObsidianConfig, SyncAdapter};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = DaemonConfig::with_defaults()?;

    // Load profiles from the user's config directory. Failures are logged
    // and the daemon boots with whatever parsed cleanly (possibly empty).
    let profiles = match levshell_config::default_profiles_dir() {
        Some(dir) => {
            tracing::info!(dir = %dir.display(), "loading profiles");
            load_profiles_from_dir(&dir)
        }
        None => {
            tracing::warn!("no XDG_CONFIG_HOME or HOME set; skipping profile load");
            Vec::new()
        }
    };
    tracing::info!(count = profiles.len(), "profiles loaded");

    let factory: ModuleFactory = Box::new(move |bus, publisher, store| {
        let context_engine = default_context_engine(publisher.clone()).with_profiles(profiles);
        let palette_providers: Vec<Box<dyn PaletteProvider>> = vec![
            Box::new(AppLauncherProvider::new()),
            Box::new(WorkspaceSwitcherProvider::new()),
            Box::new(NoteSearchProvider::new(store)),
        ];
        let palette = PaletteModule::new(publisher.clone()).with_providers(palette_providers);
        vec![
            Box::new(SwayWorkspaceModule::new(bus.clone(), publisher.clone()))
                as Box<dyn levshell_core::Module>,
            Box::new(context_engine) as Box<dyn levshell_core::Module>,
            Box::new(CpuModule::new(publisher.clone())) as Box<dyn levshell_core::Module>,
            Box::new(MemoryModule::new(publisher.clone())) as Box<dyn levshell_core::Module>,
            Box::new(BatteryModule::new(bus, publisher.clone()))
                as Box<dyn levshell_core::Module>,
            Box::new(NetworkModule::new(publisher)) as Box<dyn levshell_core::Module>,
            Box::new(palette) as Box<dyn levshell_core::Module>,
        ]
    });

    // Build the sync-adapter factory. Reads each adapter's TOML config from
    // `~/.config/levshell/sync/`; missing files mean the adapter is simply
    // not registered. Parse failures log a warning and also skip the
    // adapter — the daemon boots regardless of sync configuration health.
    let sync_factory: SyncAdapterFactory = Box::new(|| {
        let mut adapters: Vec<Arc<dyn SyncAdapter>> = Vec::new();
        let Some(sync_dir) = levshell_config::default_sync_dir() else {
            tracing::warn!("no XDG_CONFIG_HOME or HOME set; skipping sync adapter load");
            return adapters;
        };

        let obsidian_path = sync_dir.join("obsidian.toml");
        if obsidian_path.exists() {
            match ObsidianConfig::load_from(&obsidian_path) {
                Ok(cfg) => {
                    if cfg.enabled {
                        tracing::info!(
                            vault = %cfg.vault_path.display(),
                            poll_secs = cfg.poll_interval_secs,
                            "registering obsidian sync adapter"
                        );
                        adapters.push(Arc::new(ObsidianAdapter::new(cfg)));
                    } else {
                        tracing::info!("obsidian sync disabled in config");
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %obsidian_path.display(),
                        error = %e,
                        "failed to load obsidian.toml; skipping adapter"
                    );
                }
            }
        } else {
            tracing::debug!(
                path = %obsidian_path.display(),
                "no obsidian.toml found; skipping adapter"
            );
        }

        adapters
    });

    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl_c handler");
        }
        tracing::info!("ctrl-c received");
    });

    run_with_sync(config, factory, Some(sync_factory), shutdown).await
}
