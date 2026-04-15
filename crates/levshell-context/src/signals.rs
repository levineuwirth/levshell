//! Typed signal context fed into the context engine.
//!
//! A [`SignalContext`] is a bag of dotted-key signals ("workspace.name",
//! "focused.app_id", "battery.percent", …) drawn from the event bus,
//! module state, or user configuration. The cascade and expression
//! evaluator both read from the same context, so anything that can be named
//! in a [`crate::Expression`] must live in here first.
//!
//! Signals are intentionally **typed**. A comparison like `battery.percent
//! < 30` needs to know that `battery.percent` is a number, not a string —
//! type mismatches during evaluation should be programmer errors caught by
//! tests, not silent runtime coercions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The value of a signal. Wraps the four shapes the engine cares about:
/// booleans, numbers, single strings, and lists of strings (for tags).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SignalValue {
    Bool(bool),
    Number(f64),
    String(String),
    StringList(Vec<String>),
}

impl SignalValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SignalValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            SignalValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            SignalValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_string_list(&self) -> Option<&[String]> {
        match self {
            SignalValue::StringList(v) => Some(v),
            _ => None,
        }
    }

    /// Human-readable type name, used in evaluation errors.
    pub fn type_name(&self) -> &'static str {
        match self {
            SignalValue::Bool(_) => "bool",
            SignalValue::Number(_) => "number",
            SignalValue::String(_) => "string",
            SignalValue::StringList(_) => "string_list",
        }
    }
}

impl From<bool> for SignalValue {
    fn from(v: bool) -> Self {
        SignalValue::Bool(v)
    }
}

impl From<f64> for SignalValue {
    fn from(v: f64) -> Self {
        SignalValue::Number(v)
    }
}

impl From<i64> for SignalValue {
    fn from(v: i64) -> Self {
        SignalValue::Number(v as f64)
    }
}

impl From<String> for SignalValue {
    fn from(v: String) -> Self {
        SignalValue::String(v)
    }
}

impl From<&str> for SignalValue {
    fn from(v: &str) -> Self {
        SignalValue::String(v.to_owned())
    }
}

impl From<Vec<String>> for SignalValue {
    fn from(v: Vec<String>) -> Self {
        SignalValue::StringList(v)
    }
}

/// A snapshot of all named signals visible to the cascade at one point in
/// time. Cheap to clone — backed by a `HashMap`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SignalContext {
    #[serde(flatten)]
    values: HashMap<String, SignalValue>,
}

impl SignalContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` to `value`. Overwrites any existing value with the same
    /// key. Return the old value if present.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<SignalValue>) -> Option<SignalValue> {
        self.values.insert(key.into(), value.into())
    }

    /// Fluent builder variant of [`Self::set`] for constructing contexts
    /// inline in tests and rule evaluation.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<SignalValue>) -> Self {
        self.set(key, value);
        self
    }

    /// Remove a signal. Returns the removed value if present.
    pub fn remove(&mut self, key: &str) -> Option<SignalValue> {
        self.values.remove(key)
    }

    /// Look up a signal by dotted name. Returns `None` if the key is not
    /// present — the evaluator treats missing signals as falsy in boolean
    /// position and as errors in non-boolean comparisons.
    pub fn get(&self, key: &str) -> Option<&SignalValue> {
        self.values.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(|k| k.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_all_signal_value_types() {
        let ctx = SignalContext::new()
            .with("focused.app_id", "firefox")
            .with("battery.percent", 42.5_f64)
            .with("power.on_battery", true)
            .with(
                "workspace.tags",
                vec!["research".to_string(), "reading".to_string()],
            );

        assert_eq!(ctx.get("focused.app_id").and_then(|v| v.as_str()), Some("firefox"));
        assert_eq!(ctx.get("battery.percent").and_then(|v| v.as_number()), Some(42.5));
        assert_eq!(ctx.get("power.on_battery").and_then(|v| v.as_bool()), Some(true));
        let tags = ctx.get("workspace.tags").and_then(|v| v.as_string_list()).unwrap();
        assert_eq!(tags, &["research".to_string(), "reading".to_string()]);
    }

    #[test]
    fn missing_signal_returns_none() {
        let ctx = SignalContext::new();
        assert!(ctx.get("nope").is_none());
        assert!(!ctx.contains("nope"));
    }

    #[test]
    fn with_builder_overwrites_existing() {
        let ctx = SignalContext::new()
            .with("x", 1_i64)
            .with("x", 2_i64);
        assert_eq!(ctx.get("x").and_then(|v| v.as_number()), Some(2.0));
    }
}
