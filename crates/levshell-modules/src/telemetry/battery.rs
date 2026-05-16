//! Battery telemetry module.
//!
//! Scans `/sys/class/power_supply` for an entry whose `type` file contains
//! `Battery`, reads `capacity` and `status`, optionally derives a
//! time-remaining estimate from `energy_now`/`power_now`, and publishes a
//! [`WidgetUpdate`]. On AC state transitions it also emits a
//! [`Event::PowerStateChanged`] bus event so the context engine can react.
//!
//! Hosts without a battery (desktops, VMs) get [`ModuleError::Unavailable`]
//! from `start()` and the module runner parks the module — the rest of the
//! bar keeps running.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Event, EventBus, EventKind, Module, ModuleError, ModuleResult, WidgetDescriptor};
use levshell_ipc::{
    DaemonMessage, EscalationLevel, PowerState, WidgetPublisher, WidgetStatus, WidgetUpdate,
};

use crate::escalation::EscalationTracker;

/// Battery escalation only trips while *discharging* — spec example
/// "battery at 3%" implies the user is on battery power. A charging
/// pack at 3% is Ambient because it's actively recovering.
///
/// Thresholds loosely mirror the §9 examples:
/// * Aware at ≤30% (the spec's "battery drops below 30%" Aware case),
/// * Attention at ≤15%,
/// * Critical at <5% (spec's "battery at 3%").
pub fn battery_raw_escalation(percent: u8, on_battery: bool) -> EscalationLevel {
    if !on_battery {
        return EscalationLevel::Ambient;
    }
    if percent < 5 {
        EscalationLevel::Critical
    } else if percent <= 15 {
        EscalationLevel::Attention
    } else if percent <= 30 {
        EscalationLevel::Aware
    } else {
        EscalationLevel::Ambient
    }
}
use serde::{Deserialize, Serialize};

pub const BATTERY_WIDGET_ID: &str = "battery";
pub const BATTERY_WIDGET_TYPE: &str = "battery";

const TICK_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_POWER_SUPPLY_DIR: &str = "/sys/class/power_supply";

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    Charging,
    Discharging,
    NotCharging,
    Full,
    Unknown,
}

impl BatteryStatus {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Not charging" => Self::NotCharging,
            "Full" => Self::Full,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatteryState {
    pub percent: u8,
    pub status: BatteryStatus,
    pub on_battery: bool,
    pub time_remaining_seconds: Option<u64>,
}

/// Find the first directory under `base` whose `type` file contains
/// `Battery`. Returns `None` if no such directory exists (typical for
/// desktops).
pub fn find_battery_dir(base: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(type_text) = std::fs::read_to_string(path.join("type")) {
            if type_text.trim() == "Battery" {
                return Some(path);
            }
        }
    }
    None
}

/// Read a full [`BatteryState`] from a populated battery sysfs directory.
/// Returns `None` on the first missing or unparseable required file.
pub fn read_battery_state(dir: &Path) -> Option<BatteryState> {
    let percent: u8 = std::fs::read_to_string(dir.join("capacity"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let status_text = std::fs::read_to_string(dir.join("status")).ok()?;
    let status = BatteryStatus::parse(&status_text);
    let on_battery = matches!(status, BatteryStatus::Discharging);
    let time_remaining_seconds = estimate_time_remaining(dir, status);

    Some(BatteryState {
        percent,
        status,
        on_battery,
        time_remaining_seconds,
    })
}

/// Seconds until empty (discharging) or until full (charging), using
/// `energy_now`/`power_now`/`energy_full`. Returns `None` when any of the
/// required files are missing, or when the current state doesn't make an
/// estimate meaningful (e.g. `Full`, `NotCharging`).
///
/// Formula:
/// * Discharging: `energy_now / power_now * 3600`
/// * Charging:    `(energy_full - energy_now) / power_now * 3600`
///
/// `energy_*` and `power_*` are in µWh and µW respectively; the ratios
/// cancel the unit prefix.
fn estimate_time_remaining(dir: &Path, status: BatteryStatus) -> Option<u64> {
    if !matches!(
        status,
        BatteryStatus::Discharging | BatteryStatus::Charging
    ) {
        return None;
    }
    let energy_now: u64 = std::fs::read_to_string(dir.join("energy_now"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let power_now: u64 = std::fs::read_to_string(dir.join("power_now"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if power_now == 0 {
        return None;
    }
    match status {
        BatteryStatus::Discharging => {
            Some((energy_now as u128 * 3600 / power_now as u128) as u64)
        }
        BatteryStatus::Charging => {
            let energy_full: u64 = std::fs::read_to_string(dir.join("energy_full"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            let remaining = energy_full.saturating_sub(energy_now);
            Some((remaining as u128 * 3600 / power_now as u128) as u64)
        }
        _ => None,
    }
}

pub struct BatteryModule {
    bus: EventBus,
    publisher: WidgetPublisher,
    power_supply_dir: PathBuf,
    battery_dir: Option<PathBuf>,
    last_on_battery: Option<bool>,
    escalation: EscalationTracker,
}

impl BatteryModule {
    pub fn new(bus: EventBus, publisher: WidgetPublisher) -> Self {
        Self {
            bus,
            publisher,
            power_supply_dir: PathBuf::from(DEFAULT_POWER_SUPPLY_DIR),
            battery_dir: None,
            last_on_battery: None,
            escalation: EscalationTracker::new(),
        }
    }

    /// Override the sysfs base directory. Useful for tests that want to
    /// point at a tempfile tree, and for hosts that mount power_supply at
    /// a non-standard location.
    pub fn with_power_supply_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.power_supply_dir = dir.into();
        self
    }

    fn publish(&mut self, state: &BatteryState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry-battery: failed to serialize state");
                return;
            }
        };
        let outcome = self
            .escalation
            .step(battery_raw_escalation(state.percent, state.on_battery));
        let update = WidgetUpdate {
            widget_id: BATTERY_WIDGET_ID.into(),
            widget_type: BATTERY_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: outcome.level,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "telemetry-battery: failed to publish WidgetUpdate");
        }
        if outcome.entered_critical {
            self.bus.publish(Event::CriticalEscalation {
                widget_id: BATTERY_WIDGET_ID.into(),
                title: "Battery critically low".into(),
                body: format!("Battery at {}%", state.percent),
            });
        }
    }

    fn handle_sample(&mut self, state: BatteryState) {
        if self.last_on_battery != Some(state.on_battery) {
            self.bus.publish(Event::PowerStateChanged {
                on_battery: state.on_battery,
            });
            if let Err(e) = self
                .publisher
                .try_send(DaemonMessage::PowerState(PowerState {
                    on_battery: state.on_battery,
                }))
            {
                tracing::warn!(error = %e, "telemetry-battery: failed to publish PowerState");
            }
            self.last_on_battery = Some(state.on_battery);
        }
        self.publish(&state);
    }
}

#[async_trait]
impl Module for BatteryModule {
    fn name(&self) -> &str {
        "telemetry-battery"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: BATTERY_WIDGET_ID.into(),
            widget_type: BATTERY_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        // UPower watcher publishes this on AC-line changes; we re-sample
        // sysfs immediately rather than waiting up to one TICK_INTERVAL.
        vec![EventKind::AcLineChanged]
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if !matches!(event, Event::AcLineChanged { .. }) {
            return Ok(());
        }
        // No-op until start() has located the battery sysfs directory.
        let Some(dir) = self.battery_dir.as_ref() else {
            return Ok(());
        };
        if let Some(state) = read_battery_state(dir) {
            self.handle_sample(state);
        } else {
            tracing::warn!(
                path = %dir.display(),
                "telemetry-battery: AcLineChanged kick failed to read sysfs"
            );
        }
        Ok(())
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let dir = find_battery_dir(&self.power_supply_dir).ok_or_else(|| {
            ModuleError::Unavailable(format!(
                "no battery found under {}",
                self.power_supply_dir.display()
            ))
        })?;
        let state = read_battery_state(&dir).ok_or_else(|| {
            ModuleError::Failed(format!(
                "failed to read battery state at {}",
                dir.display()
            ))
        })?;
        self.battery_dir = Some(dir);
        self.handle_sample(state);
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        let dir = self
            .battery_dir
            .as_ref()
            .ok_or_else(|| ModuleError::Failed("battery directory not initialized".into()))?;
        let state = read_battery_state(dir).ok_or_else(|| {
            ModuleError::Failed(format!(
                "failed to re-read battery state at {}",
                dir.display()
            ))
        })?;
        self.handle_sample(state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn status_parser_recognizes_all_known_strings() {
        assert_eq!(BatteryStatus::parse("Charging\n"), BatteryStatus::Charging);
        assert_eq!(
            BatteryStatus::parse("Discharging"),
            BatteryStatus::Discharging
        );
        assert_eq!(BatteryStatus::parse("Full"), BatteryStatus::Full);
        assert_eq!(
            BatteryStatus::parse("Not charging"),
            BatteryStatus::NotCharging
        );
        assert_eq!(BatteryStatus::parse("bogus"), BatteryStatus::Unknown);
    }

    #[test]
    fn raw_escalation_is_ambient_while_charging_even_at_low_percent() {
        assert_eq!(
            battery_raw_escalation(3, false),
            EscalationLevel::Ambient,
            "charging battery at 3% must not escalate"
        );
        assert_eq!(battery_raw_escalation(25, false), EscalationLevel::Ambient);
    }

    #[test]
    fn raw_escalation_thresholds_match_spec_while_on_battery() {
        assert_eq!(battery_raw_escalation(100, true), EscalationLevel::Ambient);
        assert_eq!(battery_raw_escalation(31, true), EscalationLevel::Ambient);
        assert_eq!(battery_raw_escalation(30, true), EscalationLevel::Aware);
        assert_eq!(battery_raw_escalation(16, true), EscalationLevel::Aware);
        assert_eq!(battery_raw_escalation(15, true), EscalationLevel::Attention);
        assert_eq!(battery_raw_escalation(5, true), EscalationLevel::Attention);
        assert_eq!(battery_raw_escalation(4, true), EscalationLevel::Critical);
        assert_eq!(battery_raw_escalation(0, true), EscalationLevel::Critical);
    }

    #[test]
    fn finds_battery_dir_by_type_file_and_skips_mains() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        write(&base.join("AC/type"), "Mains\n");
        write(&base.join("BAT0/type"), "Battery\n");

        let found = find_battery_dir(base).unwrap();
        assert_eq!(found.file_name().unwrap(), "BAT0");
    }

    #[test]
    fn returns_none_when_no_battery_present() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("AC/type"), "Mains\n");
        assert!(find_battery_dir(dir.path()).is_none());
    }

    #[test]
    fn reads_complete_battery_state_without_energy_files() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "73\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();

        let state = read_battery_state(&bat).unwrap();
        assert_eq!(state.percent, 73);
        assert_eq!(state.status, BatteryStatus::Discharging);
        assert!(state.on_battery);
        assert_eq!(state.time_remaining_seconds, None);
    }

    #[test]
    fn estimates_time_remaining_when_discharging() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "50\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        // 50 Wh remaining drawing 10 W = 5 h = 18000 s
        std::fs::write(bat.join("energy_now"), "50000000\n").unwrap();
        std::fs::write(bat.join("power_now"), "10000000\n").unwrap();

        let state = read_battery_state(&bat).unwrap();
        assert_eq!(state.time_remaining_seconds, Some(18000));
    }

    #[test]
    fn estimates_time_to_full_when_charging() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "50\n").unwrap();
        std::fs::write(bat.join("status"), "Charging\n").unwrap();
        std::fs::write(bat.join("energy_now"), "25000000\n").unwrap();
        std::fs::write(bat.join("energy_full"), "50000000\n").unwrap();
        std::fs::write(bat.join("power_now"), "25000000\n").unwrap();
        // 25 Wh remaining to charge at 25 W = 1 h = 3600 s
        let state = read_battery_state(&bat).unwrap();
        assert_eq!(state.status, BatteryStatus::Charging);
        assert!(!state.on_battery);
        assert_eq!(state.time_remaining_seconds, Some(3600));
    }

    #[test]
    fn full_status_has_no_time_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("capacity"), "100\n").unwrap();
        std::fs::write(bat.join("status"), "Full\n").unwrap();
        let state = read_battery_state(&bat).unwrap();
        assert_eq!(state.status, BatteryStatus::Full);
        assert!(!state.on_battery);
        assert_eq!(state.time_remaining_seconds, None);
    }
}
