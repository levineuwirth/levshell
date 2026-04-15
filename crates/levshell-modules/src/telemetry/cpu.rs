//! CPU telemetry module.
//!
//! Reads `/proc/stat` every tick and turns the jiffies delta between the
//! current and previous sample into a usage percent. The pure sample
//! struct + delta math lives outside the module so the unit tests can
//! exercise it without touching the filesystem.

use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Module, ModuleError, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

pub const CPU_WIDGET_ID: &str = "cpu";
pub const CPU_WIDGET_TYPE: &str = "cpu";

const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Aggregated jiffies from the first `cpu` line of `/proc/stat`. Each
/// field is a raw counter; to get a usage percent, call
/// [`Self::usage_percent`] against a previous sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuSample {
    /// Parse the aggregate `cpu` line from a `/proc/stat` dump. Returns
    /// `None` if the input doesn't start with a recognizable `cpu` line.
    ///
    /// `iowait`/`irq`/`softirq`/`steal` are optional per the kernel docs
    /// — older kernels emit fewer columns — so missing trailing fields
    /// default to zero rather than failing the parse.
    pub fn parse_proc_stat(text: &str) -> Option<Self> {
        let first_line = text.lines().find(|l| l.starts_with("cpu "))?;
        let mut parts = first_line.split_whitespace();
        parts.next()?;
        let user = parts.next()?.parse().ok()?;
        let nice = parts.next()?.parse().ok()?;
        let system = parts.next()?.parse().ok()?;
        let idle = parts.next()?.parse().ok()?;
        let iowait = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let irq = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let softirq = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let steal = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        Some(Self {
            user,
            nice,
            system,
            idle,
            iowait,
            irq,
            softirq,
            steal,
        })
    }

    /// Total jiffies across all categories.
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    /// Idle jiffies for delta math. Includes iowait so that a disk-bound
    /// workload that happens to leave the CPU core spinning in a wait
    /// state doesn't get counted as "busy" time.
    pub fn idle_total(&self) -> u64 {
        self.idle + self.iowait
    }

    /// Percentage of CPU time not spent idle between `prev` and `self`.
    /// Clamped implicitly to [0.0, 100.0] because both deltas are bounded
    /// by `total`. Returns `0.0` on a zero delta (two samples at the same
    /// instant).
    pub fn usage_percent(&self, prev: &CpuSample) -> f64 {
        let total = self.total().saturating_sub(prev.total());
        let idle = self.idle_total().saturating_sub(prev.idle_total());
        if total == 0 {
            return 0.0;
        }
        let busy = total - idle;
        (busy as f64 / total as f64) * 100.0
    }
}

/// Serialized state payload for the `cpu` widget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuState {
    pub usage_percent: f64,
    pub load_avg_1: Option<f64>,
}

fn read_load_avg_1() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().next()?.parse().ok()
}

pub struct CpuModule {
    publisher: WidgetPublisher,
    last_sample: Option<CpuSample>,
}

impl CpuModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            last_sample: None,
        }
    }

    fn read_sample() -> ModuleResult<CpuSample> {
        let text = std::fs::read_to_string("/proc/stat")
            .map_err(|e| ModuleError::Failed(format!("reading /proc/stat: {e}")))?;
        CpuSample::parse_proc_stat(&text)
            .ok_or_else(|| ModuleError::Failed("unrecognizable /proc/stat".into()))
    }

    fn publish(&self, state: CpuState) {
        let value = match serde_json::to_value(&state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry-cpu: failed to serialize state");
                return;
            }
        };
        let update = WidgetUpdate {
            widget_id: CPU_WIDGET_ID.into(),
            widget_type: CPU_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "telemetry-cpu: failed to publish WidgetUpdate");
        }
    }
}

#[async_trait]
impl Module for CpuModule {
    fn name(&self) -> &str {
        "telemetry-cpu"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: CPU_WIDGET_ID.into(),
            widget_type: CPU_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        self.last_sample = Some(Self::read_sample()?);
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        let sample = Self::read_sample()?;
        if let Some(prev) = self.last_sample.as_ref() {
            let usage = sample.usage_percent(prev);
            self.publish(CpuState {
                usage_percent: usage,
                load_avg_1: read_load_avg_1(),
            });
        }
        self.last_sample = Some(sample);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_proc_stat_line() {
        let text = "cpu  12345 67 8901 234567 890 0 123 0 0 0\ncpu0 ...\n";
        let sample = CpuSample::parse_proc_stat(text).unwrap();
        assert_eq!(sample.user, 12345);
        assert_eq!(sample.nice, 67);
        assert_eq!(sample.system, 8901);
        assert_eq!(sample.idle, 234567);
        assert_eq!(sample.iowait, 890);
        assert_eq!(sample.irq, 0);
        assert_eq!(sample.softirq, 123);
        assert_eq!(sample.steal, 0);
    }

    #[test]
    fn parse_fills_missing_trailing_fields_with_zero() {
        let text = "cpu  100 0 0 100\n";
        let sample = CpuSample::parse_proc_stat(text).unwrap();
        assert_eq!(sample.idle, 100);
        assert_eq!(sample.iowait, 0);
        assert_eq!(sample.steal, 0);
    }

    #[test]
    fn parse_returns_none_on_garbage() {
        assert!(CpuSample::parse_proc_stat("not /proc/stat").is_none());
    }

    #[test]
    fn usage_percent_is_100_when_entire_delta_is_busy() {
        let prev = CpuSample {
            user: 0,
            nice: 0,
            system: 0,
            idle: 100,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        let curr = CpuSample {
            user: 100,
            nice: 0,
            system: 0,
            idle: 100,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        assert!((curr.usage_percent(&prev) - 100.0).abs() < 0.001);
    }

    #[test]
    fn usage_percent_is_zero_when_entire_delta_is_idle() {
        let prev = CpuSample {
            user: 0,
            nice: 0,
            system: 0,
            idle: 0,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        let curr = CpuSample {
            user: 0,
            nice: 0,
            system: 0,
            idle: 100,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        assert!(curr.usage_percent(&prev).abs() < 0.001);
    }

    #[test]
    fn usage_percent_handles_partial_busy() {
        let prev = CpuSample {
            user: 0,
            nice: 0,
            system: 0,
            idle: 0,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        let curr = CpuSample {
            user: 25,
            nice: 0,
            system: 0,
            idle: 75,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        let u = curr.usage_percent(&prev);
        assert!((u - 25.0).abs() < 0.001, "expected ~25%, got {u}");
    }

    #[test]
    fn zero_delta_returns_zero_not_nan() {
        let same = CpuSample {
            user: 100,
            nice: 0,
            system: 0,
            idle: 100,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        assert_eq!(same.usage_percent(&same), 0.0);
    }
}
