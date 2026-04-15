//! Levshell daemon entry point — a thin shim around [`levshell_daemon::run`].
//!
//! All meaningful logic lives in the library crate so the integration tests
//! can drive the same code path.

use std::pin::Pin;

use anyhow::Result;
use levshell_config::load_profiles_from_dir;
use levshell_daemon::{init_tracing, run, DaemonConfig, ModuleFactory};
use levshell_modules::{
    default_context_engine, AppLauncherProvider, BatteryModule, CpuModule, MemoryModule,
    NetworkModule, NoteSearchProvider, PaletteModule, PaletteProvider, SwayWorkspaceModule,
    WorkspaceSwitcherProvider,
};

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

    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl_c handler");
        }
        tracing::info!("ctrl-c received");
    });

    run(config, factory, shutdown).await
}
