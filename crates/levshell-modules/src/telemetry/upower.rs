//! UPower D-Bus watcher.
//!
//! Subscribes to `org.freedesktop.UPower.OnBattery` on the system bus and
//! publishes [`Event::AcLineChanged`] whenever it flips. The battery
//! module consumes that kick and immediately re-samples sysfs, giving us
//! sub-second AC-state propagation instead of waiting up to one full
//! 10s battery tick.
//!
//! Designed to fail soft: if D-Bus is unavailable (no UPower service, no
//! system bus, container without `/run/dbus`), the spawned task logs a
//! warning and exits. The 10s sysfs poll in [`BatteryModule`] is the
//! fallback in that case — nothing crashes, AC state is just slower.
//!
//! [`BatteryModule`]: crate::telemetry::BatteryModule

use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::StreamExt;
use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult, WidgetDescriptor};
use tokio::task::JoinHandle;

const UPOWER_BUS: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_IFACE: &str = "org.freedesktop.UPower";
const ON_BATTERY_PROP: &str = "OnBattery";

/// Module wrapping a tokio task that listens for UPower property changes
/// on the system bus.
pub struct UPowerWatcherModule {
    bus: EventBus,
    handle: Option<JoinHandle<()>>,
}

impl UPowerWatcherModule {
    pub fn new(bus: EventBus) -> Self {
        Self { bus, handle: None }
    }
}

#[async_trait]
impl Module for UPowerWatcherModule {
    fn name(&self) -> &str {
        "upower-watcher"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        Vec::new()
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        Vec::new()
    }

    fn tick_interval(&self) -> Option<Duration> {
        None
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let bus = self.bus.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = run_loop(bus).await {
                tracing::warn!(error = %e, "upower-watcher: D-Bus loop ended; falling back to sysfs polling");
            }
        });
        self.handle = Some(handle);
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        Ok(())
    }
}

async fn run_loop(bus: EventBus) -> Result<(), zbus::Error> {
    let conn = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(&conn, UPOWER_BUS, UPOWER_PATH, UPOWER_IFACE).await?;

    // Seed subscribers with the current value before watching for
    // changes. This keeps a hot-restarted daemon in sync with reality
    // without waiting for the next AC transition.
    if let Ok(initial) = proxy.get_property::<bool>(ON_BATTERY_PROP).await {
        bus.publish(Event::AcLineChanged {
            on_battery: initial,
        });
    }

    let mut stream = proxy.receive_property_changed::<bool>(ON_BATTERY_PROP).await;
    while let Some(change) = stream.next().await {
        match change.get().await {
            Ok(value) => bus.publish(Event::AcLineChanged { on_battery: value }),
            Err(e) => tracing::warn!(error = %e, "upower-watcher: property read failed"),
        }
    }
    Ok(())
}
