//! [`Module`] trait — the protocol every Levshell subsystem implements.
//!
//! A module owns a slice of bar functionality (workspace indicator, system
//! telemetry, command palette, ...). It declares an identity, the widgets it
//! can render, the events it wants delivered, and an optional periodic tick.
//! The [`crate::ModuleRunner`] handles its lifecycle, subscribes to the bus
//! on its behalf, and tracks its health state.
//!
//! Modules are stored as `Box<dyn Module>` for dynamic registration, so the
//! trait is dyn-compatible. We use the `async-trait` macro to box async
//! futures and keep the trait object-safe.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::bus::{Event, EventKind};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub type ModuleResult<T> = Result<T, ModuleError>;

#[derive(Debug, Error)]
pub enum ModuleError {
    /// The module's backing service is not configured or not installed.
    /// `start()` returns this to permanently mark the module unavailable;
    /// the runner skips spawning the per-module loop and excludes the
    /// module from layout calculations.
    #[error("module unavailable: {0}")]
    Unavailable(String),

    /// A transient error during start, tick, or event handling. The runner
    /// transitions the module to [`crate::HealthState::Error`] but keeps
    /// the loop alive so subsequent successful calls can recover to
    /// [`crate::HealthState::Normal`].
    #[error("module error: {0}")]
    Failed(String),
}

impl ModuleError {
    pub fn failed(msg: impl Into<String>) -> Self {
        Self::Failed(msg.into())
    }

    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self::Unavailable(msg.into())
    }
}

// ---------------------------------------------------------------------------
// Phase 0 placeholder types
// ---------------------------------------------------------------------------

/// Identity and layout slot for a widget the module can render. Phase 1 will
/// extend this with default prominence, layout group, etc.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WidgetDescriptor {
    pub id: String,
    pub widget_type: String,
}

/// A predicate that decides when a widget should be visible. Phase 1 will
/// flesh this out with the predicate DSL described in spec §3.5.3.
#[derive(Debug, Clone, Default)]
pub struct RelevanceRule;

/// JSON-schema-shaped declaration of a module's configurable parameters.
/// Phase 1 will replace this stub with the real schema type.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema;

// ---------------------------------------------------------------------------
// Module trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Module: Send + Sync + 'static {
    /// Stable identifier for this module instance. Used in logs, the
    /// subscriber name on the bus, and the per-module health state map on
    /// the runner.
    fn name(&self) -> &str;

    /// Widgets this module can render. The runner does not currently use
    /// this beyond introspection; the context engine will consume it in
    /// Phase 1.
    fn widgets(&self) -> Vec<WidgetDescriptor> {
        Vec::new()
    }

    /// Relevance rules controlling when each widget is visible.
    fn relevance(&self) -> Vec<RelevanceRule> {
        Vec::new()
    }

    /// Configuration schema for this module's TOML settings.
    fn config_schema(&self) -> ConfigSchema {
        ConfigSchema
    }

    /// Event kinds the module wants delivered. The runner subscribes on the
    /// module's behalf when it is registered. Returning an empty set is
    /// valid for purely tick-driven modules.
    fn subscribed_events(&self) -> Vec<EventKind> {
        Vec::new()
    }

    /// Optional periodic poll interval. `None` means the module is purely
    /// event-driven and `tick` will never be called. The runner uses
    /// `2 × tick_interval` as the staleness threshold per spec §5.2.2.
    fn tick_interval(&self) -> Option<Duration> {
        None
    }

    /// Capacity of the per-module event channel. Defaults to 64; modules
    /// expecting bursts can override.
    fn channel_capacity(&self) -> usize {
        64
    }

    /// Called once when the runner registers the module. Returning
    /// [`ModuleError::Unavailable`] permanently parks the module in the
    /// `Unavailable` health state.
    async fn start(&mut self) -> ModuleResult<()> {
        Ok(())
    }

    /// Called once when the runner shuts the module down. Best-effort:
    /// errors are logged but do not abort shutdown.
    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }

    /// Called for every event from [`Self::subscribed_events`] that the bus
    /// delivers. Errors transition the module to the `Error` health state
    /// but the loop continues running.
    async fn on_event(&mut self, _event: &Event) -> ModuleResult<()> {
        Ok(())
    }

    /// Called on the module's tick interval. Only invoked if
    /// [`Self::tick_interval`] returned `Some`.
    async fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}
