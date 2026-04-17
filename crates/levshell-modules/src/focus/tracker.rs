//! Pure state machine for auto-activated context profiles.
//!
//! One [`TriggerPhase`] per profile with an `auto_trigger`. The tracker
//! takes a list of `(profile_name, predicate_value, dwell, exit_dwell)`
//! tuples on every tick and emits [`TriggerAction`] decisions when a
//! dwell threshold is crossed.
//!
//! Kept entirely free of tokio and the event bus so tests can drive
//! arbitrary `Instant` timelines synchronously.
//!
//! ## Transitions
//!
//! - `Inactive` + `true`  → `Activating { since = now }`
//! - `Activating` + `true`, elapsed ≥ `dwell`  → `Active` (emit [`TriggerAction::Activate`])
//! - `Activating` + `false` → `Inactive` (cancel; no emit)
//! - `Active` + `true`  → no-op
//! - `Active` + `false` → `Deactivating { since = now }`
//! - `Deactivating` + `false`, elapsed ≥ `exit_dwell` → `Inactive` (emit [`TriggerAction::Deactivate`])
//! - `Deactivating` + `true`  → `Active` (cancel; no emit)
//!
//! ## Multi-profile tie-break
//!
//! When several profiles' triggers fire simultaneously, the tracker emits
//! an activate for each as it crosses dwell. The context engine processes
//! activations in order, so the last one wins — which matches the
//! "user-declared order in the TOML" intuition. If the caller wants
//! deterministic precedence, sort the input before passing it in.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// In-memory phase of one profile's auto-trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerPhase {
    /// Predicate is currently `false` and no activation is pending.
    Inactive,
    /// Predicate flipped to `true` at [`Activating::since`]; waiting for
    /// `dwell` to elapse before emitting an activate.
    Activating { since: Instant },
    /// Profile has been activated and the predicate remains `true`.
    Active,
    /// Predicate flipped to `false` at [`Deactivating::since`]; waiting
    /// for `exit_dwell` to elapse before emitting a deactivate.
    Deactivating { since: Instant },
}

/// One action the tracker wants the driver to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerAction {
    Activate(String),
    Deactivate(String),
}

/// Evaluated input for one profile's auto-trigger at a point in time.
#[derive(Debug, Clone)]
pub struct TriggerInput<'a> {
    pub profile: &'a str,
    pub predicate: bool,
    pub dwell: Duration,
    pub exit_dwell: Duration,
}

/// Pure tracker. One entry per profile; absent profiles (profile removed
/// from config, hot-reload etc.) get implicitly reset on next `tick`.
#[derive(Debug, Default)]
pub struct FocusModeTracker {
    phases: HashMap<String, TriggerPhase>,
}

impl FocusModeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only phase inspection for tests / diagnostics.
    pub fn phase(&self, profile: &str) -> Option<TriggerPhase> {
        self.phases.get(profile).copied()
    }

    /// Advance each tracked profile's state machine using the current
    /// predicate evaluations at `now`. Returns actions in the order they
    /// should be published on the bus.
    ///
    /// Profiles that appear in `inputs` but not yet in `phases` start at
    /// [`TriggerPhase::Inactive`]. Profiles in `phases` but absent from
    /// `inputs` (e.g. profile removed from config via hot-reload) are
    /// dropped from the tracker; if they were active we also emit a
    /// deactivate so the context engine retracts them.
    pub fn tick(&mut self, inputs: &[TriggerInput<'_>], now: Instant) -> Vec<TriggerAction> {
        let mut actions = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for input in inputs {
            seen.insert(input.profile.to_owned());
            let phase = self
                .phases
                .entry(input.profile.to_owned())
                .or_insert(TriggerPhase::Inactive);
            match (*phase, input.predicate) {
                (TriggerPhase::Inactive, true) => {
                    *phase = TriggerPhase::Activating { since: now };
                }
                (TriggerPhase::Inactive, false) => {}
                (TriggerPhase::Activating { since }, true) => {
                    if now.saturating_duration_since(since) >= input.dwell {
                        *phase = TriggerPhase::Active;
                        actions.push(TriggerAction::Activate(input.profile.to_owned()));
                    }
                }
                (TriggerPhase::Activating { .. }, false) => {
                    *phase = TriggerPhase::Inactive;
                }
                (TriggerPhase::Active, true) => {}
                (TriggerPhase::Active, false) => {
                    *phase = TriggerPhase::Deactivating { since: now };
                }
                (TriggerPhase::Deactivating { since }, false) => {
                    if now.saturating_duration_since(since) >= input.exit_dwell {
                        *phase = TriggerPhase::Inactive;
                        actions.push(TriggerAction::Deactivate(input.profile.to_owned()));
                    }
                }
                (TriggerPhase::Deactivating { .. }, true) => {
                    *phase = TriggerPhase::Active;
                }
            }
        }

        // Profile disappeared from config (hot-reload dropped it). Retract
        // if it was active; drop state either way.
        let stale: Vec<String> = self
            .phases
            .keys()
            .filter(|k| !seen.contains(k.as_str()))
            .cloned()
            .collect();
        for name in stale {
            let prev = self.phases.remove(&name);
            if matches!(prev, Some(TriggerPhase::Active) | Some(TriggerPhase::Deactivating { .. })) {
                actions.push(TriggerAction::Deactivate(name));
            }
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(profile: &'a str, predicate: bool) -> TriggerInput<'a> {
        TriggerInput {
            profile,
            predicate,
            dwell: Duration::from_secs(30),
            exit_dwell: Duration::from_secs(60),
        }
    }

    #[test]
    fn initial_true_starts_activating_but_does_not_emit() {
        let mut t = FocusModeTracker::new();
        let now = Instant::now();
        let actions = t.tick(&[input("writing", true)], now);
        assert!(actions.is_empty());
        assert!(matches!(t.phase("writing"), Some(TriggerPhase::Activating { .. })));
    }

    #[test]
    fn sustained_true_past_dwell_emits_activate() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(&[input("writing", true)], t0);
        // Still activating at dwell - 1.
        let a29 = t.tick(&[input("writing", true)], t0 + Duration::from_secs(29));
        assert!(a29.is_empty());
        // Crosses dwell at 30.
        let a30 = t.tick(&[input("writing", true)], t0 + Duration::from_secs(30));
        assert_eq!(a30, vec![TriggerAction::Activate("writing".into())]);
        assert_eq!(t.phase("writing"), Some(TriggerPhase::Active));
    }

    #[test]
    fn flicker_during_activating_cancels_without_emit() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(&[input("writing", true)], t0);
        let a = t.tick(&[input("writing", false)], t0 + Duration::from_secs(10));
        assert!(a.is_empty());
        assert_eq!(t.phase("writing"), Some(TriggerPhase::Inactive));
    }

    #[test]
    fn sustained_false_past_exit_dwell_emits_deactivate() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        // Activate first.
        t.tick(&[input("writing", true)], t0);
        t.tick(&[input("writing", true)], t0 + Duration::from_secs(30));
        // Predicate flips false.
        let a0 = t.tick(&[input("writing", false)], t0 + Duration::from_secs(60));
        assert!(a0.is_empty());
        assert!(matches!(t.phase("writing"), Some(TriggerPhase::Deactivating { .. })));
        // Still deactivating short of exit_dwell.
        let a59 = t.tick(&[input("writing", false)], t0 + Duration::from_secs(119));
        assert!(a59.is_empty());
        // Crosses exit_dwell (60 + 60 = 120).
        let a60 = t.tick(&[input("writing", false)], t0 + Duration::from_secs(120));
        assert_eq!(a60, vec![TriggerAction::Deactivate("writing".into())]);
        assert_eq!(t.phase("writing"), Some(TriggerPhase::Inactive));
    }

    #[test]
    fn flicker_during_deactivating_cancels_without_emit() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(&[input("writing", true)], t0);
        t.tick(&[input("writing", true)], t0 + Duration::from_secs(30));
        t.tick(&[input("writing", false)], t0 + Duration::from_secs(60));
        let a = t.tick(&[input("writing", true)], t0 + Duration::from_secs(70));
        assert!(a.is_empty());
        assert_eq!(t.phase("writing"), Some(TriggerPhase::Active));
    }

    #[test]
    fn multiple_profiles_track_independently() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(
            &[input("writing", true), input("lit-review", false)],
            t0,
        );
        let a = t.tick(
            &[input("writing", true), input("lit-review", false)],
            t0 + Duration::from_secs(30),
        );
        assert_eq!(a, vec![TriggerAction::Activate("writing".into())]);
        assert_eq!(t.phase("lit-review"), Some(TriggerPhase::Inactive));
    }

    #[test]
    fn removed_profile_emits_deactivate_if_active() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(&[input("writing", true)], t0);
        t.tick(&[input("writing", true)], t0 + Duration::from_secs(30));
        // Next tick: profile no longer in the config (hot-reload).
        let a = t.tick(&[], t0 + Duration::from_secs(31));
        assert_eq!(a, vec![TriggerAction::Deactivate("writing".into())]);
        assert_eq!(t.phase("writing"), None);
    }

    #[test]
    fn removed_inactive_profile_is_silently_dropped() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        t.tick(&[input("writing", false)], t0);
        let a = t.tick(&[], t0 + Duration::from_secs(1));
        assert!(a.is_empty());
        assert_eq!(t.phase("writing"), None);
    }

    #[test]
    fn per_profile_dwell_values_are_respected() {
        let mut t = FocusModeTracker::new();
        let t0 = Instant::now();
        let quick = TriggerInput {
            profile: "quick",
            predicate: true,
            dwell: Duration::from_secs(5),
            exit_dwell: Duration::from_secs(10),
        };
        t.tick(std::slice::from_ref(&quick), t0);
        // Crosses its own 5-second dwell.
        let a = t.tick(&[quick], t0 + Duration::from_secs(5));
        assert_eq!(a, vec![TriggerAction::Activate("quick".into())]);
    }
}

