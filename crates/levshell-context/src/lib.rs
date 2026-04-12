//! Levshell context engine.
//!
//! Resolves the user's *current context* (workspace, focused window, time,
//! activity, project) into a layered context profile that drives widget
//! visibility and module behavior. Implements the five-step conflict
//! resolution algorithm and hysteresis to prevent flicker.

#![forbid(unsafe_code)]
