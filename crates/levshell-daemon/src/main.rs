//! Levshell daemon entry point — a thin shim around [`levshell_daemon::run`].
//!
//! All meaningful logic lives in the library crate so the integration tests
//! can drive the same code path.

use std::pin::Pin;

use anyhow::Result;
use levshell_daemon::{init_tracing, run, DaemonConfig, ModuleFactory};
use levshell_modules::SwayWorkspaceModule;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = DaemonConfig::with_defaults()?;

    let factory: ModuleFactory = Box::new(|bus, publisher| {
        vec![Box::new(SwayWorkspaceModule::new(bus, publisher)) as Box<dyn levshell_core::Module>]
    });

    let shutdown: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl_c handler");
        }
        tracing::info!("ctrl-c received");
    });

    run(config, factory, shutdown).await
}
