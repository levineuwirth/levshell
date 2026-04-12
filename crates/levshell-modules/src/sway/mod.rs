//! Sway-specific built-in modules.
//!
//! `indicator` holds the pure state-computation logic for the workspace
//! indicator widget; `module` is the production [`Module`] that actually
//! talks to swayipc and emits widget updates.
//!
//! [`Module`]: levshell_core::Module

pub mod indicator;
pub mod module;

pub use indicator::{
    WorkspaceIndicatorState, WorkspaceInfo, WORKSPACE_WIDGET_ID, WORKSPACE_WIDGET_TYPE,
};
pub use module::SwayWorkspaceModule;
