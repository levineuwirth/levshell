//! System telemetry modules.
//!
//! Each submodule is one [`levshell_core::Module`] that polls a `/proc` or
//! `/sys` file on a fixed tick interval and publishes a [`WidgetUpdate`] to
//! the shell. The pure parsing logic (which is what the unit tests
//! exercise) lives alongside the plumbing so each file reads top-to-bottom:
//! types → parser → module.
//!
//! ## Module roster (Phase 1.3)
//!
//! | Module           | Source                | Tick  | Widget id |
//! |------------------|-----------------------|-------|-----------|
//! | [`CpuModule`]    | `/proc/stat`          |  2s   | `cpu`     |
//! | [`MemoryModule`] | `/proc/meminfo`       |  2s   | `memory`  |
//! | [`BatteryModule`]| `/sys/class/power_supply` | 10s | `battery` |
//! | [`NetworkModule`]| `/proc/net/dev`, `/proc/net/wireless` | 5s | `network` |
//! | [`DiskModule`]   | `statvfs(2)` per mount |  60s | `disk`    |
//!
//! ## Design notes
//!
//! * All four modules use **blocking `std::fs` reads**. Procfs and sysfs
//!   are virtual filesystems backed by in-kernel generators; their reads
//!   never block on disk I/O, so routing them through `tokio::fs` would
//!   only add a thread-pool hop for no benefit.
//!
//! * Modules that need a delta between two samples (`CpuModule`,
//!   `NetworkModule`) record a **baseline sample in `start()`** so the
//!   first `tick()` can publish a meaningful value. Modules that publish
//!   an absolute value (`MemoryModule`, `BatteryModule`) send their first
//!   update directly from `start()`.
//!
//! * Missing hardware returns [`levshell_core::ModuleError::Unavailable`]
//!   from `start()`. The module runner parks the module in the
//!   `Unavailable` health state and the rest of the bar keeps running —
//!   desktops without a battery, hosts without wifi, etc.
//!
//! [`WidgetUpdate`]: levshell_ipc::WidgetUpdate

pub mod battery;
pub mod cpu;
pub mod disk;
pub mod memory;
pub mod network;
pub mod power_profiles;
pub mod upower;

pub use battery::{
    BatteryModule, BatteryState, BatteryStatus, BATTERY_WIDGET_ID, BATTERY_WIDGET_TYPE,
};
pub use disk::{
    DiskConfig, DiskModule, DiskState, MountUsage, DISK_WIDGET_ID, DISK_WIDGET_TYPE,
};
pub use cpu::{CpuModule, CpuSample, CpuState, CPU_WIDGET_ID, CPU_WIDGET_TYPE};
pub use memory::{MemoryModule, MemoryState, MEMORY_WIDGET_ID, MEMORY_WIDGET_TYPE};
pub use network::{
    classify_latency, IfaceCounters, IfaceRate, LinkQuality, NetworkConfig, NetworkModule,
    NetworkState, NETWORK_WIDGET_ID, NETWORK_WIDGET_TYPE,
};
pub use power_profiles::{
    next_profile, order_profiles, PowerProfileState, PowerProfilesModule,
    POWER_PROFILE_WIDGET_ID, POWER_PROFILE_WIDGET_TYPE,
};
pub use upower::UPowerWatcherModule;
