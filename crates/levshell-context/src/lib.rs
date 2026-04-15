//! Levshell context engine.
//!
//! Resolves the user's *current context* (workspace, focused window, module
//! state) into a deterministic widget layout. Implements the five-step
//! conflict resolution algorithm (spec §3.5) and a hysteresis layer that
//! prevents flicker when input signals oscillate.
//!
//! The engine is split into a **pure core** (this crate's modules) and a
//! **runtime driver** (the `ContextEngineModule` in `levshell-modules`, to be
//! added in Phase 1.2 step D). Everything in this crate is synchronous,
//! deterministic, and exhaustively unit-testable. The runtime driver owns
//! bus subscriptions, publishes IPC messages, and holds the engine state
//! behind an async boundary.
//!
//! ## Modules
//!
//! - [`signals`] — typed input signals (workspace, focused app, module state)
//! - [`expr`] — expression DSL AST, tokenizer, recursive-descent parser
//! - [`rules`] — compiled relevance rules and profile overrides
//! - [`cascade`] — the pure five-step resolver
//! - [`hysteresis`] — debounce layer over the cascade
//! - [`error`] — crate-wide error type

#![forbid(unsafe_code)]

pub mod cascade;
pub mod error;
pub mod expr;
pub mod hysteresis;
pub mod rules;
pub mod signals;

pub use cascade::{resolve_layout, CascadeInput, Layout, WidgetWidthFn};
pub use error::{ContextError, Result};
pub use expr::{parse_expression, Expression};
pub use hysteresis::{Hysteresis, HysteresisConfig, Transition};
pub use rules::{CompiledRule, Profile, WidgetDef};
pub use signals::{SignalContext, SignalValue};

// Re-export the canonical Prominence type from the IPC crate so downstream
// code that works with cascade outputs doesn't need a second dependency.
pub use levshell_ipc::Prominence;
