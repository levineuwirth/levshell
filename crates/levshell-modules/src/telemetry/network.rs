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
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use levshell_core::{Module, ModuleError, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

pub const NETWORK_WIDGET_ID: &str = "network";
pub const NETWORK_WIDGET_TYPE: &str = "network";

const TICK_INTERVAL: Duration = Duration::from_secs(5);
/// Connect timeout for the latency probe. Anything slower than this is
/// indistinguishable from "down" for interactive work (SSH, web).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_PROBE_TARGET: &str = "1.1.1.1:443";

/// End-to-end link quality, from a TCP-connect round-trip to a fixed
/// host (spec §2.3.3 "connection quality indicator"). This is the
/// *reachability* signal `/proc/net/wireless` can't give: a full-bars
/// wifi association behind a dead uplink still reads `Poor`/`Down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkQuality {
    Good,
    Fair,
    Poor,
    Down,
}

/// Map a probe round-trip to a quality bucket. `None` (connect failed or
/// timed out) is `Down`. Thresholds target *interactive* usability — a
/// remote shell stays comfortable under ~80 ms and tolerable under
/// ~250 ms; beyond that every keystrokes-to-echo hurts.
pub fn classify_latency(rtt: Option<Duration>) -> LinkQuality {
    match rtt {
        None => LinkQuality::Down,
        Some(d) if d < Duration::from_millis(80) => LinkQuality::Good,
        Some(d) if d < Duration::from_millis(250) => LinkQuality::Fair,
        Some(_) => LinkQuality::Poor,
    }
}

/// `~/.config/levshell/modules/network.toml`. Absent → probe Cloudflare
/// 1.1.1.1:443 every 30 s. Set `latency_target = ""` to disable the
/// probe entirely (air-gapped / privacy).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub latency_target: String,
    pub probe_secs: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            latency_target: DEFAULT_PROBE_TARGET.to_owned(),
            probe_secs: 30,
        }
    }
}

impl NetworkConfig {
    pub fn load_from_dir(dir: &Path) -> Self {
        let path = dir.join("network.toml");
        match std::fs::read_to_string(&path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "network.toml malformed; using probe defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// `None` when the probe is disabled (empty target).
    fn probe_target(&self) -> Option<&str> {
        let t = self.latency_target.trim();
        (!t.is_empty()).then_some(t)
    }

    fn probe_interval(&self) -> Duration {
        Duration::from_secs(self.probe_secs.max(5))
    }
}

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
    /// Last probe round-trip in ms; `None` when the probe is disabled or
    /// hasn't completed a cycle yet.
    pub latency_ms: Option<u64>,
    /// `None` when probing is disabled — the shell then shows no dot
    /// rather than a misleading `Down`.
    pub quality: Option<LinkQuality>,
}

pub struct NetworkModule {
    publisher: WidgetPublisher,
    last_sample: Option<(Instant, HashMap<String, IfaceCounters>)>,
    config: NetworkConfig,
    /// When the next latency probe is due. The probe runs on a slower
    /// cadence than the byte-rate tick so it stays a polite single
    /// connect every `probe_secs`, not every 5 s.
    next_probe: Instant,
    /// Last probe result, carried across the ticks between probes so the
    /// dot doesn't flicker off.
    last_quality: Option<(Option<u64>, LinkQuality)>,
}

impl NetworkModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self::with_config(publisher, NetworkConfig::default())
    }

    pub fn with_config(publisher: WidgetPublisher, config: NetworkConfig) -> Self {
        Self {
            publisher,
            last_sample: None,
            config,
            next_probe: Instant::now(),
            last_quality: None,
        }
    }

    /// TCP-connect probe: time how long a fresh connection to `target`
    /// takes, capped at [`PROBE_TIMEOUT`]. A connect is enough — we want
    /// reachability + RTT, not throughput, and it needs no privileges
    /// (unlike ICMP). DNS resolution, if any, is part of the measured
    /// time, which is fair: a dead resolver is a degraded link.
    async fn probe(target: &str) -> Option<Duration> {
        let start = Instant::now();
        match tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::net::TcpStream::connect(target),
        )
        .await
        {
            Ok(Ok(_stream)) => Some(start.elapsed()),
            Ok(Err(e)) => {
                tracing::debug!(target, error = %e, "telemetry-network: probe connect failed");
                None
            }
            Err(_) => None, // timed out
        }
    }

    /// Run a probe if one is due, update the cached result, and return
    /// `(latency_ms, quality)` for the current state. Returns `(None,
    /// None)` when probing is disabled.
    async fn refresh_quality(&mut self) -> (Option<u64>, Option<LinkQuality>) {
        let Some(target) = self.config.probe_target() else {
            return (None, None);
        };
        if Instant::now() >= self.next_probe {
            let target = target.to_owned();
            let rtt = Self::probe(&target).await;
            let quality = classify_latency(rtt);
            let ms = rtt.map(|d| d.as_millis().min(u64::MAX as u128) as u64);
            self.last_quality = Some((ms, quality));
            self.next_probe = Instant::now() + self.config.probe_interval();
        }
        match self.last_quality {
            Some((ms, q)) => (ms, Some(q)),
            None => (None, None),
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
        let (latency_ms, quality) = self.refresh_quality().await;
        self.publish(&NetworkState {
            interfaces,
            latency_ms,
            quality,
        });
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
    fn classify_latency_buckets() {
        assert_eq!(classify_latency(None), LinkQuality::Down);
        assert_eq!(
            classify_latency(Some(Duration::from_millis(5))),
            LinkQuality::Good
        );
        assert_eq!(
            classify_latency(Some(Duration::from_millis(79))),
            LinkQuality::Good
        );
        assert_eq!(
            classify_latency(Some(Duration::from_millis(80))),
            LinkQuality::Fair
        );
        assert_eq!(
            classify_latency(Some(Duration::from_millis(249))),
            LinkQuality::Fair
        );
        assert_eq!(
            classify_latency(Some(Duration::from_millis(250))),
            LinkQuality::Poor
        );
        assert_eq!(
            classify_latency(Some(Duration::from_secs(3))),
            LinkQuality::Poor
        );
    }

    #[test]
    fn empty_target_disables_probe() {
        let cfg = NetworkConfig {
            latency_target: "  ".into(),
            probe_secs: 30,
        };
        assert!(cfg.probe_target().is_none());
    }

    #[test]
    fn default_config_probes_cloudflare() {
        let cfg = NetworkConfig::default();
        assert_eq!(cfg.probe_target(), Some("1.1.1.1:443"));
        assert_eq!(cfg.probe_interval(), Duration::from_secs(30));
    }

    #[test]
    fn probe_interval_has_a_floor() {
        // A 0/1-second config would hammer the endpoint; clamp to 5 s.
        let cfg = NetworkConfig {
            latency_target: "x:1".into(),
            probe_secs: 1,
        };
        assert_eq!(cfg.probe_interval(), Duration::from_secs(5));
    }

    #[test]
    fn missing_network_toml_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NetworkConfig::load_from_dir(dir.path());
        assert_eq!(cfg.latency_target, "1.1.1.1:443");
    }

    #[test]
    fn quality_to_percent_maps_range_endpoints() {
        assert_eq!(quality_to_percent(0), 0);
        assert_eq!(quality_to_percent(70), 100);
        assert_eq!(quality_to_percent(35), 50);
        assert_eq!(quality_to_percent(100), 100);
    }
}
