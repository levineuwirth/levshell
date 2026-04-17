//! Compiled relevance rules, widget descriptors, and context profiles.
//!
//! A rule ties an [`Expression`] (a parsed predicate over the signal
//! context) to a target widget and a prominence level. At runtime the
//! cascade walks a flat list of rules, evaluates each against the current
//! [`crate::SignalContext`], and keeps the highest-prominence winner per
//! widget. Profiles sit one layer above — they unconditionally override a
//! set of widgets with user-declared prominence values.

use std::collections::HashMap;
use std::time::Duration;

use crate::expr::{evaluate, Expression};
use crate::signals::SignalContext;
use levshell_ipc::Prominence;

/// Metadata about a bar widget that the cascade needs to make layout
/// decisions. One [`WidgetDef`] exists per unique `widget_id`; usually
/// produced by a module's `widgets()` method and a pinch of user config.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetDef {
    pub id: String,
    pub widget_type: String,
    /// Fallback prominence if no rule or profile applies.
    pub default_prominence: Prominence,
    /// Global priority. Higher values are demoted last when the bar runs
    /// out of space. The cascade treats this as a strict total order; ties
    /// are broken by the `id` string (stable, deterministic).
    pub priority: i32,
    /// Bar zone for display ordering. Not used by the cascade's demotion
    /// logic in Phase 1.2 — only echoed back in the output layout.
    pub zone: Zone,
}

impl WidgetDef {
    pub fn new(id: impl Into<String>, widget_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            widget_type: widget_type.into(),
            default_prominence: Prominence::Visible,
            priority: 0,
            zone: Zone::Right,
        }
    }

    pub fn with_default(mut self, p: Prominence) -> Self {
        self.default_prominence = p;
        self
    }

    pub fn with_priority(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }

    pub fn with_zone(mut self, z: Zone) -> Self {
        self.zone = z;
        self
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Zone {
    Left,
    Center,
    Right,
}

/// A compiled, ready-to-evaluate relevance rule. Produced once at module
/// registration time; the cascade evaluates it on every layout tick.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// The widget this rule targets. Must match a [`WidgetDef::id`] the
    /// cascade knows about, otherwise the rule is silently skipped.
    pub target: String,
    /// The predicate. When it evaluates to `true`, the rule fires.
    pub when: Expression,
    /// The prominence the rule wants to apply to `target` when it fires.
    pub prominence: Prominence,
    /// Optional per-rule activation delay override, for hysteresis. A
    /// `None` uses the engine's global default.
    pub activation_delay: Option<Duration>,
    /// Optional per-rule deactivation delay override.
    pub deactivation_delay: Option<Duration>,
    /// Source label for logs/diagnostics (e.g. `"ssh-dashboard:in_research"`).
    pub source: String,
}

impl CompiledRule {
    pub fn new(
        target: impl Into<String>,
        when: Expression,
        prominence: Prominence,
    ) -> Self {
        Self {
            target: target.into(),
            when,
            prominence,
            activation_delay: None,
            deactivation_delay: None,
            source: String::new(),
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_delays(
        mut self,
        activation: Option<Duration>,
        deactivation: Option<Duration>,
    ) -> Self {
        self.activation_delay = activation;
        self.deactivation_delay = deactivation;
        self
    }

    /// Evaluate the rule against the given signal context. Returns `true`
    /// if the rule fires (and the cascade should consider its prominence).
    /// An error from the underlying expression evaluator is mapped to
    /// `false` — a broken rule should never crash the engine, just log and
    /// skip.
    pub fn evaluate(&self, ctx: &SignalContext) -> bool {
        match evaluate(&self.when, ctx) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    source = %self.source,
                    target = %self.target,
                    error = %e,
                    "relevance rule evaluation error; treating as false"
                );
                false
            }
        }
    }
}

/// A named context profile. Profiles unconditionally override widget
/// prominence for the widgets they mention; widgets they don't mention
/// fall through to whatever the relevance layer decided.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub name: String,
    pub overrides: HashMap<String, Prominence>,
    /// Optional: suppress notifications while this profile is active.
    /// Echoed in the output layout but not consumed by the cascade itself.
    pub suppress_notifications: bool,
    /// Optional auto-activation trigger. When present, a focus-mode driver
    /// (see `levshell-modules::focus`) watches signals and requests profile
    /// activation when the predicate has been continuously true for
    /// `dwell`, deactivation when it has been continuously false for
    /// `exit_dwell`. Absent → profile is manual-only (ctl / keybind).
    pub auto_trigger: Option<AutoTrigger>,
}

/// Auto-activation rule for a context profile (spec §2.12.4 literature
/// review mode, §2.12.5 writing mode). Deliberately signal-agnostic: the
/// predicate is any [`Expression`] over the runtime [`SignalContext`], so
/// the profile can react to `focused.app_id`, window titles, workspace
/// names, tags, battery state, or anything else the engine can name.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoTrigger {
    /// Predicate evaluated on every tick + signal change. `true` means the
    /// conditions for this profile to be active are met *right now*.
    pub when: Expression,
    /// Sustained-true duration required before publishing an activate
    /// request. Prevents profile flicker when the user briefly alt-tabs
    /// through a matching window.
    pub dwell: Duration,
    /// Sustained-false duration required before publishing a deactivate
    /// request. Typically longer than `dwell` so quick sidebars (reply to
    /// a slack ping, check mail) don't drop the user out of lit-review /
    /// writing mode immediately.
    pub exit_dwell: Duration,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            overrides: HashMap::new(),
            suppress_notifications: false,
            auto_trigger: None,
        }
    }

    pub fn with(mut self, widget_id: impl Into<String>, prominence: Prominence) -> Self {
        self.overrides.insert(widget_id.into(), prominence);
        self
    }

    pub fn with_auto_trigger(mut self, trigger: AutoTrigger) -> Self {
        self.auto_trigger = Some(trigger);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse_expression;

    #[test]
    fn compiled_rule_evaluates_true_when_predicate_matches() {
        let ctx = SignalContext::new().with("focused.app_id", "firefox");
        let rule = CompiledRule::new(
            "workspace-indicator",
            parse_expression(r#"focused.app_id == "firefox""#).unwrap(),
            Prominence::Compact,
        );
        assert!(rule.evaluate(&ctx));
    }

    #[test]
    fn compiled_rule_swallows_eval_errors_as_false() {
        let ctx = SignalContext::new().with("focused.app_id", "firefox");
        // Comparing a string signal with a numeric op → eval error → false.
        let rule = CompiledRule::new(
            "widget",
            parse_expression(r#"focused.app_id < 5"#).unwrap(),
            Prominence::Visible,
        );
        assert!(!rule.evaluate(&ctx));
    }

    #[test]
    fn widget_def_builders_compose() {
        let w = WidgetDef::new("cpu", "system_telemetry")
            .with_default(Prominence::Compact)
            .with_priority(10)
            .with_zone(Zone::Right);
        assert_eq!(w.id, "cpu");
        assert_eq!(w.default_prominence, Prominence::Compact);
        assert_eq!(w.priority, 10);
        assert_eq!(w.zone, Zone::Right);
    }

    #[test]
    fn profile_builder_sets_overrides() {
        let p = Profile::new("writing")
            .with("cpu", Prominence::Badge)
            .with("notifications", Prominence::Hidden);
        assert_eq!(p.overrides.get("cpu"), Some(&Prominence::Badge));
        assert_eq!(p.overrides.get("notifications"), Some(&Prominence::Hidden));
    }
}
