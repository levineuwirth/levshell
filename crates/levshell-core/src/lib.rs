//! Levshell core runtime primitives.
//!
//! Defines the typed [`Event`] enum, the in-process [`EventBus`] (mpsc fan-out),
//! the [`Module`] trait that every Levshell subsystem implements, and the
//! [`ModuleRunner`] that owns module lifecycle, ticks, and health-state
//! transitions. This crate is intentionally free of internal dependencies so
//! that recompiling a leaf crate does not cascade through the workspace.

#![forbid(unsafe_code)]

mod bus;
mod module;
mod runner;

pub use bus::{Event, EventBus, EventKind, SubscriberStats};
pub use module::{
    ConfigSchema, Module, ModuleError, ModuleResult, RelevanceRule, WidgetDescriptor,
};
pub use runner::{HealthState, ModuleHandle, ModuleRunner};
