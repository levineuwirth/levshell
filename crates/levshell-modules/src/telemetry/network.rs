//! Network telemetry module.
//!
//! Reads `/proc/net/dev` every tick and computes per-interface rx/tx
//! byte-rates from the delta against the previous sample. Optionally
//! merges in link-quality readings from `/proc/net/wireless` for wireless
//! interfaces. Loopback (`lo`) is always excluded from the output.
//!
//! The module publishes a single [`WidgetUpdate`] containing a list of
//! interface rates; the shell is free to display them all or pick the one
//! currently carrying the default route. Phase 1.3 doesn't try to detect
//! the default route itself — that requires `/proc/net/route` parsing and
//! adds complexity for marginal value.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use levshell_core::{Module, ModuleError, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

pub const NETWORK_WIDGET_ID: &str = "network";
pub const NETWORK_WIDGET_TYPE: &str = "network";

const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// Raw byte counters for one interface, as reported by `/proc/net/dev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfaceCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Parse `/proc/net/dev`. The first two header lines have no colon so
/// they fall through the `split_once` guard. Loopback is explicitly
/// excluded. Lines with fewer than 16 numeric fields (invalid /proc/net/dev
/// format) are skipped silently.
pub fn parse_proc_net_dev(text: &str) -> HashMap<String, IfaceCounters> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 16 {
            continue;
        }
        let Ok(rx) = fields[0].parse::<u64>() else {
            continue;
        };
        let Ok(tx) = fields[8].parse::<u64>() else {
            continue;
        };
        out.insert(
            name.to_owned(),
            IfaceCounters {
                rx_bytes: rx,
                tx_bytes: tx,
            },
        );
    }
    out
}

/// Parse `/proc/net/wireless` into a map of interface → link quality.
/// Header lines don't contain colons and fall through cleanly; data lines
/// are recognised by the second whitespace field parsing as a number
/// (the `link` column in the wireless quality report).
pub fn parse_proc_net_wireless(text: &str) -> HashMap<String, u8> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let quality_str = fields[1].trim_end_matches('.');
        let Ok(q) = quality_str.parse::<f64>() else {
            continue;
        };
        out.insert(name.to_owned(), q.clamp(0.0, 255.0) as u8);
    }
    out
}

/// Convert a raw `/proc/net/wireless` link quality (typically 0..70) to
/// a 0..100 percent.
pub fn quality_to_percent(q: u8) -> u8 {
    ((q.min(70) as u16 * 100) / 70) as u8
}

/// Computed rx/tx rate for one interface, plus optional wireless quality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IfaceRate {
    pub name: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub quality_percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkState {
    pub interfaces: Vec<IfaceRate>,
}

pub struct NetworkModule {
    publisher: WidgetPublisher,
    last_sample: Option<(Instant, HashMap<String, IfaceCounters>)>,
}

impl NetworkModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            last_sample: None,
        }
    }

    fn read_proc_net_dev() -> ModuleResult<HashMap<String, IfaceCounters>> {
        let text = std::fs::read_to_string("/proc/net/dev")
            .map_err(|e| ModuleError::Failed(format!("reading /proc/net/dev: {e}")))?;
        Ok(parse_proc_net_dev(&text))
    }

    fn read_wireless_quality() -> HashMap<String, u8> {
        std::fs::read_to_string("/proc/net/wireless")
            .map(|t| parse_proc_net_wireless(&t))
            .unwrap_or_default()
    }

    fn publish(&self, state: &NetworkState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry-network: failed to serialize state");
                return;
            }
        };
        let update = WidgetUpdate {
            widget_id: NETWORK_WIDGET_ID.into(),
            widget_type: NETWORK_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: EscalationLevel::Ambient,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "telemetry-network: failed to publish WidgetUpdate");
        }
    }
}

#[async_trait]
impl Module for NetworkModule {
    fn name(&self) -> &str {
        "telemetry-network"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: NETWORK_WIDGET_ID.into(),
            widget_type: NETWORK_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let sample = Self::read_proc_net_dev()?;
        self.last_sample = Some((Instant::now(), sample));
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        let sample = Self::read_proc_net_dev()?;
        let now = Instant::now();
        let qualities = Self::read_wireless_quality();

        let interfaces = match self.last_sample.as_ref() {
            Some((prev_time, prev_sample)) => {
                let dt = now.duration_since(*prev_time).as_secs_f64().max(0.001);
                let mut rates = Vec::with_capacity(sample.len());
                for (name, curr) in &sample {
                    let prev = prev_sample.get(name).copied().unwrap_or(IfaceCounters {
                        rx_bytes: 0,
                        tx_bytes: 0,
                    });
                    let rx_bps =
                        (curr.rx_bytes.saturating_sub(prev.rx_bytes) as f64 / dt) as u64;
                    let tx_bps =
                        (curr.tx_bytes.saturating_sub(prev.tx_bytes) as f64 / dt) as u64;
                    let quality_percent =
                        qualities.get(name).copied().map(quality_to_percent);
                    rates.push(IfaceRate {
                        name: name.clone(),
                        rx_bps,
                        tx_bps,
                        quality_percent,
                    });
                }
                rates.sort_by(|a, b| a.name.cmp(&b.name));
                rates
            }
            None => Vec::new(),
        };

        self.last_sample = Some((now, sample));
        self.publish(&NetworkState { interfaces });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_NET_DEV: &str = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:   12345      30    0    0    0     0          0         0   12345      30    0    0    0     0       0          0
  eth0: 9876543    1000    0    0    0     0          0         0  1234567     500    0    0    0     0       0          0
 wlan0:  555555     200    0    0    0     0          0         0   666666     250    0    0    0     0       0          0
";

    #[test]
    fn parse_dev_skips_loopback_and_keeps_real_interfaces() {
        let ifaces = parse_proc_net_dev(PROC_NET_DEV);
        assert_eq!(ifaces.len(), 2);
        assert!(ifaces.contains_key("eth0"));
        assert!(ifaces.contains_key("wlan0"));
        assert!(!ifaces.contains_key("lo"));
        assert_eq!(ifaces["eth0"].rx_bytes, 9_876_543);
        assert_eq!(ifaces["eth0"].tx_bytes, 1_234_567);
        assert_eq!(ifaces["wlan0"].rx_bytes, 555_555);
    }

    #[test]
    fn parse_dev_skips_malformed_lines() {
        let text = "garbage\neth0: only three fields\n";
        let ifaces = parse_proc_net_dev(text);
        assert!(ifaces.is_empty());
    }

    const PROC_NET_WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
 wlan0: 0000   54.  -56.  -256        0      0      0     22      6        0
";

    #[test]
    fn parse_wireless_reads_link_quality() {
        let q = parse_proc_net_wireless(PROC_NET_WIRELESS);
        assert_eq!(q.get("wlan0"), Some(&54));
    }

    #[test]
    fn parse_wireless_skips_header_lines_without_colons() {
        let q = parse_proc_net_wireless(PROC_NET_WIRELESS);
        // Header lines "Inter-| sta-|..." and " face | tus |..." contain
        // no colon, so they must not show up as keys.
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn quality_to_percent_maps_range_endpoints() {
        assert_eq!(quality_to_percent(0), 0);
        assert_eq!(quality_to_percent(70), 100);
        assert_eq!(quality_to_percent(35), 50);
        assert_eq!(quality_to_percent(100), 100);
    }
}
