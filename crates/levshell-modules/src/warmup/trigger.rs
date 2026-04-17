//! Pure trigger decision for warmup mode.
//!
//! Kept separate from the [`Module`](levshell_core::Module) shell so tests
//! can drive arbitrary `Instant` / `DateTime<Utc>` timelines without an
//! async runtime. The module threads real clocks in; tests thread fake
//! ones in.
//!
//! ## The heuristic
//!
//! Warmup fires when **all three** conditions hold on a sway event at
//! time `t`:
//!
//! 1. There was a gap — either this is the first observed event since
//!    daemon startup, or the gap to the last event is ≥ `gap_secs`.
//! 2. The last warmup stamp is older than `gap_secs`, or there is no
//!    last stamp at all.
//! 3. OR, if `calendar_day_trigger` is enabled AND the last warmup
//!    stamp is in a strictly earlier calendar day than `t` (UTC).
//!
//! Condition 3 is an OR with 1+2 — it's a permissive "also fire on
//! day boundary" switch, off by default per user preference.

use std::time::Instant;

use chrono::{DateTime, Utc};

use super::config::WarmupConfig;

/// In-memory piece of the trigger state. Combined with the persisted
/// `last_warmup_at` to make a fire decision.
#[derive(Debug, Default)]
pub struct TriggerState {
    /// When we last observed a sway event (or received an explicit
    /// "user is active" signal). `None` at daemon startup → first
    /// event always counts as a gap.
    pub last_activity_at: Option<Instant>,
}

impl TriggerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an activity timestamp, returning whether warmup should
    /// fire.
    ///
    /// - `now_instant` / `now_utc` — the monotonic and wall-clock
    ///   times of the activity. Split so tests can fake them
    ///   independently and so the persistence layer can use wall-clock
    ///   while gap checks use monotonic.
    /// - `last_warmup_at` — persisted stamp of the last fire.
    ///
    /// On a positive decision the caller should publish the warmup
    /// payload, stamp `last_warmup_at = now_utc`, and persist.
    pub fn decide(
        &mut self,
        now_instant: Instant,
        now_utc: DateTime<Utc>,
        last_warmup_at: Option<DateTime<Utc>>,
        config: &WarmupConfig,
    ) -> bool {
        if !config.enabled {
            self.last_activity_at = Some(now_instant);
            return false;
        }

        let fire = would_fire(
            self.last_activity_at,
            now_instant,
            now_utc,
            last_warmup_at,
            config,
        );

        self.last_activity_at = Some(now_instant);
        fire
    }
}

fn would_fire(
    last_activity: Option<Instant>,
    now_instant: Instant,
    now_utc: DateTime<Utc>,
    last_warmup_at: Option<DateTime<Utc>>,
    config: &WarmupConfig,
) -> bool {
    let gap = config.gap();

    // Calendar-day branch: if enabled, fires whenever last_warmup_at is
    // in a strictly earlier UTC calendar day, regardless of activity gap.
    // The goal is a once-a-day ramp-up for users who want it.
    if config.calendar_day_trigger {
        let day_crossed = match last_warmup_at {
            Some(prev) => prev.date_naive() < now_utc.date_naive(),
            None => true,
        };
        if day_crossed {
            return true;
        }
    }

    // Gap branch: need a gap AND the previous fire can't be within the
    // last `gap` window (otherwise ~4h of continuous nothing-then-use
    // would fire, stop, and fire again 4h later even if user was busy).
    let had_gap = match last_activity {
        None => true,
        Some(t) => now_instant.saturating_duration_since(t) >= gap,
    };
    if !had_gap {
        return false;
    }

    match last_warmup_at {
        None => true,
        Some(prev) => {
            let since = now_utc.signed_duration_since(prev);
            since.num_seconds().max(0) as u64 >= config.gap_secs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;

    fn cfg() -> WarmupConfig {
        WarmupConfig::default() // gap_secs = 14400, no day trigger
    }

    fn cfg_with_day_trigger() -> WarmupConfig {
        WarmupConfig {
            calendar_day_trigger: true,
            ..Default::default()
        }
    }

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn fresh_daemon_fires_when_never_warmed() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        assert!(s.decide(base, dt(2026, 4, 17, 9, 0), None, &cfg()));
    }

    #[test]
    fn fresh_daemon_does_not_fire_within_recent_warmup() {
        // Daemon restarted 1h after a warmup fired. First event should
        // not re-fire.
        let mut s = TriggerState::new();
        let base = Instant::now();
        let recent = dt(2026, 4, 17, 8, 0);
        let now = dt(2026, 4, 17, 9, 0);
        assert!(!s.decide(base, now, Some(recent), &cfg()));
    }

    #[test]
    fn continuous_use_never_fires() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        // Prime the state with an initial event (treated as a gap — fires).
        let first_utc = dt(2026, 4, 17, 9, 0);
        s.decide(base, first_utc, None, &cfg());
        // Simulate 6 hours of continuous use, one event per minute.
        let mut fires = 0;
        for i in 1..=360 {
            let ti = base + Duration::from_secs(i * 60);
            let utc_i = first_utc + chrono::Duration::minutes(i as i64);
            // last_warmup_at is the first fire — pass it in so condition 2
            // blocks re-fires.
            if s.decide(ti, utc_i, Some(first_utc), &cfg()) {
                fires += 1;
            }
        }
        assert_eq!(fires, 0, "continuous use must not fire mid-session");
    }

    #[test]
    fn return_after_long_gap_fires() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        let first_utc = dt(2026, 4, 17, 9, 0);
        // First event at 09:00, fires.
        assert!(s.decide(base, first_utc, None, &cfg()));
        // Next event at 14:00 (5h later), should re-fire since both gap
        // and since-last-warmup exceed 4h.
        let t5 = base + 5 * HOUR;
        let utc5 = first_utc + chrono::Duration::hours(5);
        assert!(s.decide(t5, utc5, Some(first_utc), &cfg()));
    }

    #[test]
    fn return_within_threshold_does_not_fire() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        let first_utc = dt(2026, 4, 17, 9, 0);
        s.decide(base, first_utc, None, &cfg());
        // Gap of 3h — below the 4h threshold.
        let t3 = base + 3 * HOUR;
        let utc3 = first_utc + chrono::Duration::hours(3);
        assert!(!s.decide(t3, utc3, Some(first_utc), &cfg()));
    }

    #[test]
    fn disabled_config_never_fires() {
        let mut s = TriggerState::new();
        let cfg = WarmupConfig {
            enabled: false,
            ..Default::default()
        };
        let base = Instant::now();
        assert!(!s.decide(base, dt(2026, 4, 17, 9, 0), None, &cfg));
    }

    #[test]
    fn calendar_day_trigger_fires_on_day_crossing_without_gap() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        let day1 = dt(2026, 4, 17, 23, 50);
        // Warmed up yesterday (10 min ago).
        assert!(!s.decide(base, day1, Some(day1), &cfg_with_day_trigger()));
        // 20 min later — crosses midnight UTC. Gap is only 20min, but
        // day_trigger fires anyway.
        let next = base + Duration::from_secs(20 * 60);
        let next_utc = dt(2026, 4, 18, 0, 10);
        assert!(s.decide(next, next_utc, Some(day1), &cfg_with_day_trigger()));
    }

    #[test]
    fn calendar_day_trigger_off_does_not_fire_on_day_crossing() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        let day1 = dt(2026, 4, 17, 23, 50);
        s.decide(base, day1, Some(day1), &cfg());
        let next = base + Duration::from_secs(20 * 60);
        let next_utc = dt(2026, 4, 18, 0, 10);
        // Default config: day_trigger off. Short gap → no fire.
        assert!(!s.decide(next, next_utc, Some(day1), &cfg()));
    }

    #[test]
    fn decide_stamps_activity_even_when_not_firing() {
        let mut s = TriggerState::new();
        let base = Instant::now();
        let first_utc = dt(2026, 4, 17, 9, 0);
        s.decide(base, first_utc, Some(first_utc), &cfg());
        assert!(s.last_activity_at.is_some());
    }
}
