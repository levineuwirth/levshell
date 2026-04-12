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

pub mod sway;

pub use sway::{
    SwayWorkspaceModule, WorkspaceIndicatorState, WorkspaceInfo, WORKSPACE_WIDGET_ID,
    WORKSPACE_WIDGET_TYPE,
};
