//! Shared urgency escalation state machine per spec design §9.
//!
//! Each telemetry module computes a *raw* [`EscalationLevel`] from its
//! current sample (e.g., CPU ≥95% → `Critical`). The raw level is fed
//! into an [`EscalationTracker`] which enforces the two spec rules:
//!
//! 1. **Gradual escalation.** A widget may only climb by one level per
//!    tick — a raw jump from `Ambient` to `Critical` is rendered as
//!    `Ambient → Aware → Attention → Critical` over three polls. This
//!    prevents a transient spike from flashing the bar red without the
//!    user seeing the warning build up.
//!
//! 2. **Fast de-escalation.** The raw level is adopted immediately on
//!    the way down. Once the underlying condition clears, the widget
//!    drops to its true level on the next tick.
//!
//! The tracker also reports when a step crossed *into* `Critical` so
//! callers can emit the one-time notification required by spec rule 3.

use levshell_ipc::EscalationLevel;

/// Per-widget escalation state. Construct once per widget, call
/// [`Self::step`] every publish cycle with the raw level derived from
/// the latest sample.
#[derive(Debug, Clone, Default)]
pub struct EscalationTracker {
    current: EscalationLevel,
}

/// Result of one [`EscalationTracker::step`] call.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// The effective level after hysteresis — publish this.
    pub level: EscalationLevel,
    /// `true` on the single tick that crossed from a lower level into
    /// [`EscalationLevel::Critical`]. Caller emits a one-shot
    /// notification on this edge.
    pub entered_critical: bool,
}

impl EscalationTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> EscalationLevel {
        self.current
    }

    /// Advance one tick given the current raw level derived from the
    /// latest sample. Returns the effective level the widget should
    /// render at, plus whether this tick was a fresh Critical edge.
    pub fn step(&mut self, raw: EscalationLevel) -> TickOutcome {
        let prev = self.current;
        let next = if raw > prev { step_up(prev) } else { raw };
        self.current = next;
        TickOutcome {
            level: next,
            entered_critical: next == EscalationLevel::Critical
                && prev != EscalationLevel::Critical,
        }
    }
}

fn step_up(level: EscalationLevel) -> EscalationLevel {
    match level {
        EscalationLevel::Ambient => EscalationLevel::Aware,
        EscalationLevel::Aware => EscalationLevel::Attention,
        EscalationLevel::Attention => EscalationLevel::Critical,
        EscalationLevel::Critical => EscalationLevel::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_starts_at_ambient() {
        let t = EscalationTracker::new();
        assert_eq!(t.current(), EscalationLevel::Ambient);
    }

    #[test]
    fn ambient_raw_stays_ambient() {
        let mut t = EscalationTracker::new();
        let out = t.step(EscalationLevel::Ambient);
        assert_eq!(out.level, EscalationLevel::Ambient);
        assert!(!out.entered_critical);
    }

    #[test]
    fn raw_critical_from_ambient_steps_to_aware_only() {
        let mut t = EscalationTracker::new();
        let out = t.step(EscalationLevel::Critical);
        assert_eq!(out.level, EscalationLevel::Aware);
        assert!(!out.entered_critical);
    }

    #[test]
    fn three_ticks_at_critical_reaches_critical() {
        let mut t = EscalationTracker::new();
        assert_eq!(t.step(EscalationLevel::Critical).level, EscalationLevel::Aware);
        assert_eq!(
            t.step(EscalationLevel::Critical).level,
            EscalationLevel::Attention
        );
        let out = t.step(EscalationLevel::Critical);
        assert_eq!(out.level, EscalationLevel::Critical);
        assert!(out.entered_critical, "first Critical entry must flag edge");
    }

    #[test]
    fn sustained_critical_does_not_re_flag_entry() {
        let mut t = EscalationTracker::new();
        for _ in 0..3 {
            t.step(EscalationLevel::Critical);
        }
        assert_eq!(t.current(), EscalationLevel::Critical);
        let out = t.step(EscalationLevel::Critical);
        assert_eq!(out.level, EscalationLevel::Critical);
        assert!(
            !out.entered_critical,
            "subsequent Critical ticks must not re-emit"
        );
    }

    #[test]
    fn drop_from_critical_is_immediate() {
        let mut t = EscalationTracker::new();
        for _ in 0..3 {
            t.step(EscalationLevel::Critical);
        }
        let out = t.step(EscalationLevel::Ambient);
        assert_eq!(out.level, EscalationLevel::Ambient);
        assert!(!out.entered_critical);
    }

    #[test]
    fn drop_to_intermediate_level_is_immediate() {
        let mut t = EscalationTracker::new();
        for _ in 0..3 {
            t.step(EscalationLevel::Critical);
        }
        let out = t.step(EscalationLevel::Aware);
        assert_eq!(out.level, EscalationLevel::Aware);
    }

    #[test]
    fn re_entering_critical_after_drop_flags_edge_again() {
        let mut t = EscalationTracker::new();
        for _ in 0..3 {
            t.step(EscalationLevel::Critical);
        }
        t.step(EscalationLevel::Ambient);
        // Climb back up.
        t.step(EscalationLevel::Critical);
        t.step(EscalationLevel::Critical);
        let out = t.step(EscalationLevel::Critical);
        assert_eq!(out.level, EscalationLevel::Critical);
        assert!(
            out.entered_critical,
            "re-entry into Critical after a drop must re-emit edge"
        );
    }

    #[test]
    fn single_step_rise_applies_raw_directly() {
        let mut t = EscalationTracker::new();
        // Ambient → Aware is a single step, so raw is adopted.
        assert_eq!(t.step(EscalationLevel::Aware).level, EscalationLevel::Aware);
        // Aware → Attention is also single-step.
        assert_eq!(
            t.step(EscalationLevel::Attention).level,
            EscalationLevel::Attention
        );
    }

    #[test]
    fn plateau_below_current_tracks_raw() {
        let mut t = EscalationTracker::new();
        t.step(EscalationLevel::Attention); // → Aware
        t.step(EscalationLevel::Attention); // → Attention
        let out = t.step(EscalationLevel::Aware);
        assert_eq!(out.level, EscalationLevel::Aware);
    }
}
