//! Memory telemetry module.
//!
//! Reads `/proc/meminfo` every tick, extracts `MemTotal` and `MemAvailable`,
//! computes used % from the pair, and publishes a [`WidgetUpdate`]. Unlike
//! [`super::cpu::CpuModule`] this produces an absolute reading on each
//! sample, so there's no delta state to maintain — and `start()` can
//! publish an initial value immediately rather than waiting for the first
//! tick.

use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Event, EventBus, Module, ModuleError, ModuleResult, WidgetDescriptor};
use levshell_ipc::{
    DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate,
};
use serde::{Deserialize, Serialize};

use crate::escalation::EscalationTracker;

pub const MEMORY_WIDGET_ID: &str = "memory";
pub const MEMORY_WIDGET_TYPE: &str = "memory";

const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Same thresholds as CPU — headroom is a finite resource and 95% used
/// memory is a pre-OOM warning.
pub fn memory_raw_escalation(used_percent: f64) -> EscalationLevel {
    if used_percent >= 95.0 {
        EscalationLevel::Critical
    } else if used_percent >= 85.0 {
        EscalationLevel::Attention
    } else if used_percent >= 60.0 {
        EscalationLevel::Aware
    } else {
        EscalationLevel::Ambient
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryState {
    pub total_kb: u64,
    pub available_kb: u64,
    pub used_kb: u64,
    pub used_percent: f64,
}

impl MemoryState {
    /// Parse `/proc/meminfo` text into a [`MemoryState`]. Ignores lines
    /// other than `MemTotal` and `MemAvailable`; returns `None` if either
    /// is missing.
    pub fn parse_proc_meminfo(text: &str) -> Option<Self> {
        let mut total = None;
        let mut available = None;
        for line in text.lines() {
            let Some((k, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(v_str) = rest.split_whitespace().next() else {
                continue;
            };
            let Ok(v) = v_str.parse::<u64>() else {
                continue;
            };
            match k {
                "MemTotal" => total = Some(v),
                "MemAvailable" => available = Some(v),
                _ => {}
            }
            if total.is_some() && available.is_some() {
                break;
            }
        }
        let total_kb = total?;
        let available_kb = available?;
        let used_kb = total_kb.saturating_sub(available_kb);
        let used_percent = if total_kb == 0 {
            0.0
        } else {
            (used_kb as f64 / total_kb as f64) * 100.0
        };
        Some(Self {
            total_kb,
            available_kb,
            used_kb,
            used_percent,
        })
    }
}

pub struct MemoryModule {
    bus: EventBus,
    publisher: WidgetPublisher,
    escalation: EscalationTracker,
}

impl MemoryModule {
    pub fn new(bus: EventBus, publisher: WidgetPublisher) -> Self {
        Self {
            bus,
            publisher,
            escalation: EscalationTracker::new(),
        }
    }

    fn read_state() -> ModuleResult<MemoryState> {
        let text = std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| ModuleError::Failed(format!("reading /proc/meminfo: {e}")))?;
        MemoryState::parse_proc_meminfo(&text)
            .ok_or_else(|| ModuleError::Failed("unrecognizable /proc/meminfo".into()))
    }

    fn publish(&mut self, state: MemoryState) {
        let value = match serde_json::to_value(&state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry-memory: failed to serialize state");
                return;
            }
        };
        let outcome = self
            .escalation
            .step(memory_raw_escalation(state.used_percent));
        let update = WidgetUpdate {
            widget_id: MEMORY_WIDGET_ID.into(),
            widget_type: MEMORY_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: outcome.level,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "telemetry-memory: failed to publish WidgetUpdate");
        }
        if outcome.entered_critical {
            self.bus.publish(Event::CriticalEscalation {
                widget_id: MEMORY_WIDGET_ID.into(),
                title: "Memory critically high".into(),
                body: format!("Memory at {:.0}%", state.used_percent),
            });
        }
    }
}

#[async_trait]
impl Module for MemoryModule {
    fn name(&self) -> &str {
        "telemetry-memory"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: MEMORY_WIDGET_ID.into(),
            widget_type: MEMORY_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let state = Self::read_state()?;
        self.publish(state);
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        let state = Self::read_state()?;
        self.publish(state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "MemTotal:       16334288 kB
MemFree:         1000000 kB
MemAvailable:   11234560 kB
Buffers:         200000 kB
Cached:          5000000 kB
";

    #[test]
    fn parses_realistic_meminfo_sample() {
        let s = MemoryState::parse_proc_meminfo(SAMPLE).unwrap();
        assert_eq!(s.total_kb, 16_334_288);
        assert_eq!(s.available_kb, 11_234_560);
        assert_eq!(s.used_kb, 16_334_288 - 11_234_560);
        let expected_pct = ((16_334_288u64 - 11_234_560) as f64 / 16_334_288.0) * 100.0;
        assert!((s.used_percent - expected_pct).abs() < 0.001);
    }

    #[test]
    fn returns_none_when_mem_available_missing() {
        let text = "MemTotal:       16334288 kB\n";
        assert!(MemoryState::parse_proc_meminfo(text).is_none());
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(MemoryState::parse_proc_meminfo("not meminfo").is_none());
    }

    #[test]
    fn raw_escalation_thresholds_match_spec() {
        assert_eq!(memory_raw_escalation(0.0), EscalationLevel::Ambient);
        assert_eq!(memory_raw_escalation(59.9), EscalationLevel::Ambient);
        assert_eq!(memory_raw_escalation(60.0), EscalationLevel::Aware);
        assert_eq!(memory_raw_escalation(84.9), EscalationLevel::Aware);
        assert_eq!(memory_raw_escalation(85.0), EscalationLevel::Attention);
        assert_eq!(memory_raw_escalation(94.9), EscalationLevel::Attention);
        assert_eq!(memory_raw_escalation(95.0), EscalationLevel::Critical);
        assert_eq!(memory_raw_escalation(100.0), EscalationLevel::Critical);
    }

    #[test]
    fn skips_malformed_lines_without_aborting() {
        let text = "\
garbage line with no colon
MemTotal:       16000000 kB
corrupt: not a number
MemAvailable:    8000000 kB
";
        let s = MemoryState::parse_proc_meminfo(text).unwrap();
        assert_eq!(s.total_kb, 16_000_000);
        assert_eq!(s.available_kb, 8_000_000);
        assert_eq!(s.used_kb, 8_000_000);
        assert!((s.used_percent - 50.0).abs() < 0.001);
    }
}
