//! Levshell built-in modules.
//!
//! Each top-bar feature (workspace indicator, system telemetry, notification
//! center, command palette, calendar hub, ...) is a [`Module`] implementation
//! that lives here. Modules read from `levshell-data`, subscribe to events on
//! the bus from `levshell-core`, and emit widget state patches that the
//! daemon forwards to the QML shell over `levshell-ipc`.
//!
//! [`Module`]: levshell_core::Module

#![forbid(unsafe_code)]

pub mod context_engine;
pub mod palette;
pub mod sway;
pub mod telemetry;

pub use context_engine::{default_context_engine, default_widgets, ContextEngineModule};
pub use palette::{
    AppLauncherProvider, NoteSearchProvider, PaletteItem, PaletteModule, PaletteProvider,
    PaletteState, WorkspaceSwitcherProvider, PALETTE_WIDGET_ID, PALETTE_WIDGET_TYPE,
};
pub use sway::{
    SwayWorkspaceModule, WorkspaceIndicatorState, WorkspaceInfo, WORKSPACE_WIDGET_ID,
    WORKSPACE_WIDGET_TYPE,
};
pub use telemetry::{
    BatteryModule, BatteryState, BatteryStatus, CpuModule, CpuSample, CpuState, IfaceRate,
    MemoryModule, MemoryState, NetworkModule, NetworkState, BATTERY_WIDGET_ID,
    BATTERY_WIDGET_TYPE, CPU_WIDGET_ID, CPU_WIDGET_TYPE, MEMORY_WIDGET_ID, MEMORY_WIDGET_TYPE,
    NETWORK_WIDGET_ID, NETWORK_WIDGET_TYPE,
};
