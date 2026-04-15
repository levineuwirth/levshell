//! Debounce layer that prevents rapid prominence oscillation.
//!
//! The cascade alone is memoryless: given inputs, it returns a layout. But
//! the spec (§3.5.5) requires that prominence changes only "commit" after
//! a condition has held steady for a configured delay. A noisy signal
//! (e.g. CPU bouncing around 60%) would otherwise cause widgets to flap
//! between prominence levels every tick.
//!
//! [`Hysteresis`] sits in front of a cascade result stream:
//!
//! 1. Caller feeds it a fresh cascade output at time `now`.
//! 2. For each widget whose new prominence differs from the *committed*
//!    value, Hysteresis records a pending change with an expiration time
//!    (`now + activation_delay` for promotion, `now + deactivation_delay`
//!    for demotion).
//! 3. At each call, any pending change whose expiration has passed is
//!    committed. Pending changes whose target prominence no longer matches
//!    the incoming cascade output are discarded.
//!
//! The clock is an injected function so tests can drive time manually.
//! Production wires [`std::time::Instant::now`] behind a closure.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use levshell_ipc::Prominence;

/// Hysteresis configuration. Defaults match the spec (§3.5.5): 2s to
/// activate (promote), 10s to deactivate (demote).
#[derive(Debug, Clone, Copy)]
pub struct HysteresisConfig {
    pub activation_delay: Duration,
    pub deactivation_delay: Duration,
}

impl Default for HysteresisConfig {
    fn default() -> Self {
        Self {
            activation_delay: Duration::from_secs(2),
            deactivation_delay: Duration::from_secs(10),
        }
    }
}

/// A pending prominence transition that hasn't been committed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub target: Prominence,
    pub commit_at: Instant,
}

/// State-ful debouncer over cascade outputs.
#[derive(Debug)]
pub struct Hysteresis {
    config: HysteresisConfig,
    committed: HashMap<String, Prominence>,
    pending: HashMap<String, Transition>,
}

impl Hysteresis {
    pub fn new(config: HysteresisConfig) -> Self {
        Self {
            config,
            committed: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(HysteresisConfig::default())
    }

    /// Read-only accessors for tests and diagnostics.
    pub fn committed(&self) -> &HashMap<String, Prominence> {
        &self.committed
    }

    pub fn pending(&self) -> &HashMap<String, Transition> {
        &self.pending
    }

    /// Observe a fresh cascade result at `now` and return the current
    /// committed prominences. The returned map reflects only values that
    /// have passed the debounce threshold; in-flight transitions are
    /// tracked in [`Self::pending`] but do not appear in the result.
    ///
    /// Widgets not present in `cascade` are left alone — the engine never
    /// "forgets" a widget, so the caller can drop prominence for removed
    /// widgets by explicitly committing them to [`Prominence::Hidden`]
    /// beforehand.
    pub fn observe(
        &mut self,
        cascade: &HashMap<String, Prominence>,
        now: Instant,
    ) -> HashMap<String, Prominence> {
        // Step 1: commit any pending transitions whose expiration has passed
        // and whose target still matches the incoming cascade.
        let mut ready: Vec<String> = Vec::new();
        for (widget, transition) in &self.pending {
            if transition.commit_at > now {
                continue;
            }
            if cascade.get(widget) == Some(&transition.target) {
                ready.push(widget.clone());
            }
        }
        for widget in ready {
            let transition = self.pending.remove(&widget).expect("just listed");
            self.committed.insert(widget, transition.target);
        }

        // Step 2: for each widget in the new cascade, decide whether to
        // start, update, or discard a pending transition.
        for (widget, &target) in cascade {
            let current = self.committed.get(widget).copied();
            match current {
                None => {
                    // First time we've seen this widget — commit
                    // immediately. There's no prior state to hysteresize.
                    self.committed.insert(widget.clone(), target);
                    self.pending.remove(widget);
                }
                Some(committed) if committed == target => {
                    // No change; drop any stale pending transition toward
                    // a different target.
                    self.pending.remove(widget);
                }
                Some(committed) => {
                    let delay = if target > committed {
                        self.config.activation_delay
                    } else {
                        self.config.deactivation_delay
                    };
                    let deadline = now + delay;

                    match self.pending.get(widget) {
                        Some(pending) if pending.target == target => {
                            // Already pending the same target — keep the
                            // earlier deadline intact so the change
                            // commits at the originally-scheduled time.
                        }
                        _ => {
                            self.pending.insert(
                                widget.clone(),
                                Transition {
                                    target,
                                    commit_at: deadline,
                                },
                            );
                        }
                    }
                }
            }
        }

        self.committed.clone()
    }

    /// Force-commit the current state for `widget_id` to `prominence`,
    /// bypassing hysteresis. Used by the user-override path and by tests.
    pub fn force_commit(&mut self, widget_id: impl Into<String>, prominence: Prominence) {
        let id = widget_id.into();
        self.pending.remove(&id);
        self.committed.insert(id, prominence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cascade(pairs: &[(&str, Prominence)]) -> HashMap<String, Prominence> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect()
    }

    fn cfg(activation: u64, deactivation: u64) -> HysteresisConfig {
        HysteresisConfig {
            activation_delay: Duration::from_millis(activation),
            deactivation_delay: Duration::from_millis(deactivation),
        }
    }

    #[test]
    fn first_observation_commits_immediately() {
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();
        let out = h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);
        assert_eq!(out.get("cpu"), Some(&Prominence::Compact));
        assert!(h.pending().is_empty());
    }

    #[test]
    fn promotion_waits_for_activation_delay() {
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();

        // Initial commit at Compact.
        h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);

        // New cascade wants Visible. Should stay pending.
        let out = h.observe(&cascade(&[("cpu", Prominence::Visible)]), t0);
        assert_eq!(out.get("cpu"), Some(&Prominence::Compact));
        assert_eq!(h.pending().get("cpu").map(|t| t.target), Some(Prominence::Visible));

        // 500ms later — still pending.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Visible)]),
            t0 + Duration::from_millis(500),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Compact));

        // 1000ms later — deadline hit, commit.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Visible)]),
            t0 + Duration::from_millis(1000),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Visible));
        assert!(h.pending().is_empty());
    }

    #[test]
    fn demotion_uses_deactivation_delay() {
        // activation=100ms, deactivation=1000ms. A promotion is fast, a
        // demotion is slow.
        let mut h = Hysteresis::new(cfg(100, 1000));
        let t0 = Instant::now();
        h.observe(&cascade(&[("cpu", Prominence::Visible)]), t0);

        // Demote to Compact — pending for 1000ms.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Compact)]),
            t0 + Duration::from_millis(500),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Visible));

        // 500ms later — still pending.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Compact)]),
            t0 + Duration::from_millis(1000),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Visible));

        // 1600ms total — past the 1500ms deadline.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Compact)]),
            t0 + Duration::from_millis(1600),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Compact));
    }

    #[test]
    fn oscillating_signal_does_not_flap() {
        // Activation and deactivation both 1000ms. An input that bounces
        // between Compact and Visible every 100ms should never commit a
        // change.
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();
        h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);

        for i in 1..20 {
            let next = if i % 2 == 0 {
                Prominence::Compact
            } else {
                Prominence::Visible
            };
            let out = h.observe(
                &cascade(&[("cpu", next)]),
                t0 + Duration::from_millis(i * 100),
            );
            assert_eq!(
                out.get("cpu"),
                Some(&Prominence::Compact),
                "flapped at iteration {i}"
            );
        }
    }

    #[test]
    fn pending_transition_cancels_if_cascade_reverts() {
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();
        h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);

        // Pending Visible.
        h.observe(&cascade(&[("cpu", Prominence::Visible)]), t0);
        assert!(h.pending().contains_key("cpu"));

        // Cascade reverts to Compact before the deadline — pending is
        // cleared.
        let out = h.observe(
            &cascade(&[("cpu", Prominence::Compact)]),
            t0 + Duration::from_millis(500),
        );
        assert_eq!(out.get("cpu"), Some(&Prominence::Compact));
        assert!(h.pending().is_empty());
    }

    #[test]
    fn force_commit_bypasses_hysteresis() {
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();
        h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);

        h.force_commit("cpu", Prominence::Expanded);
        assert_eq!(h.committed().get("cpu"), Some(&Prominence::Expanded));
        assert!(h.pending().is_empty());
    }

    #[test]
    fn pending_deadline_preserved_across_identical_observations() {
        let mut h = Hysteresis::new(cfg(1000, 1000));
        let t0 = Instant::now();
        h.observe(&cascade(&[("cpu", Prominence::Compact)]), t0);

        // Open a pending Visible at t0+0.
        h.observe(&cascade(&[("cpu", Prominence::Visible)]), t0);
        let deadline = h.pending().get("cpu").unwrap().commit_at;

        // Observe again at t0+500 — same target, so the deadline must not
        // reset.
        h.observe(
            &cascade(&[("cpu", Prominence::Visible)]),
            t0 + Duration::from_millis(500),
        );
        assert_eq!(h.pending().get("cpu").unwrap().commit_at, deadline);
    }
}
