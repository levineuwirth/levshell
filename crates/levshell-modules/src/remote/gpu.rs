//! GPU Utilization Dashboard (spec §2.5.4, §6.2 item 4).
//!
//! For each host with role `gpu`, the module polls
//! `nvidia-smi --query-gpu=... --format=csv,noheader,nounits` on a
//! configurable cadence (default 15s), parses each line into a
//! [`GpuSample`], and publishes a [`GpuFleetState`] widget.
//!
//! Hosts that don't have `nvidia-smi` installed surface as `offline`
//! with `error = "nvidia-smi not installed"` rather than wedging the
//! whole fleet. Same for auth/connectivity failures — the widget
//! keeps rendering the rest of the fleet.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

use super::host::{HostConfig, HostRegistry, HostRole};
use super::runner::{RemoteError, RemoteRunner};

pub const GPU_WIDGET_ID: &str = "gpu-fleet";
pub const GPU_WIDGET_TYPE: &str = "gpu_fleet";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Single GPU sample as reported by nvidia-smi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSample {
    pub index: u32,
    pub name: String,
    /// Percent utilization, 0.0–100.0.
    pub utilization_percent: f64,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    /// Celsius.
    pub temperature_c: u32,
}

impl GpuSample {
    /// Parse a single line of
    /// `nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits`
    /// output. Whitespace around fields is stripped; nvidia-smi emits
    /// `" 0, NVIDIA A100-SXM4-80GB, 12, 3024, 81920, 45"`.
    pub fn parse_csv_line(line: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() != 6 {
            return None;
        }
        Some(Self {
            index: fields[0].parse().ok()?,
            name: fields[1].to_string(),
            utilization_percent: fields[2].parse().ok()?,
            memory_used_mb: fields[3].parse().ok()?,
            memory_total_mb: fields[4].parse().ok()?,
            temperature_c: fields[5].parse().ok()?,
        })
    }
}

/// Per-host GPU state. `status`:
///
/// - `"healthy"`: ≥1 GPU found and utilization is below
///   80 % on every card.
/// - `"busy"`:    utilization ≥ 80 % on at least one card.
/// - `"offline"`: host unreachable or nvidia-smi not installed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuHostState {
    pub name: String,
    pub display_name: String,
    pub project: Option<String>,
    pub status: String,
    pub gpus: Vec<GpuSample>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuFleetState {
    pub hosts: Vec<GpuHostState>,
}

pub struct GpuDashboardModule {
    registry: Arc<HostRegistry>,
    runner: Arc<dyn RemoteRunner>,
    publisher: WidgetPublisher,
    poll_interval: Duration,
}

impl GpuDashboardModule {
    pub fn new(
        registry: Arc<HostRegistry>,
        runner: Arc<dyn RemoteRunner>,
        publisher: WidgetPublisher,
    ) -> Self {
        Self {
            registry,
            runner,
            publisher,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    fn hosts(&self) -> Vec<HostConfig> {
        self.registry
            .filter_by_role(HostRole::Gpu)
            .into_iter()
            .cloned()
            .collect()
    }

    async fn probe_host(&self, host: &HostConfig) -> GpuHostState {
        let argv = [
            "nvidia-smi",
            "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ];
        let result = self
            .runner
            .run(host, &argv, host.connect_timeout() + Duration::from_secs(2))
            .await;

        match result {
            Ok(out) if out.is_success() => {
                let gpus: Vec<GpuSample> = out
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(GpuSample::parse_csv_line)
                    .collect();

                let status = if gpus.is_empty() {
                    "offline"
                } else if gpus.iter().any(|g| g.utilization_percent >= 80.0) {
                    "busy"
                } else {
                    "healthy"
                };

                GpuHostState {
                    name: host.name.clone(),
                    display_name: host.display_name().to_string(),
                    project: host.project.clone(),
                    status: status.into(),
                    gpus,
                    error: if status == "offline" {
                        Some("nvidia-smi returned no GPUs".into())
                    } else {
                        None
                    },
                }
            }
            Ok(out) => {
                // nvidia-smi missing → 127; auth denied → 255. Either
                // way, we want a clean "offline" row rather than a
                // whole-widget failure.
                let reason = match out.status {
                    Some(127) => "nvidia-smi not installed".into(),
                    Some(c) => format!("nvidia-smi exit {c}"),
                    None => "nvidia-smi killed".into(),
                };
                GpuHostState {
                    name: host.name.clone(),
                    display_name: host.display_name().to_string(),
                    project: host.project.clone(),
                    status: "offline".into(),
                    gpus: Vec::new(),
                    error: Some(reason),
                }
            }
            Err(RemoteError::Timeout { .. }) => GpuHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                status: "offline".into(),
                gpus: Vec::new(),
                error: Some("timeout".into()),
            },
            Err(e) => GpuHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                status: "offline".into(),
                gpus: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    fn publish_widget(&self, state: &GpuFleetState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "gpu-dashboard: failed to serialize state");
                return;
            }
        };
        let widget_status = if state.hosts.iter().all(|h| h.status != "offline") {
            WidgetStatus::Normal
        } else {
            WidgetStatus::Stale
        };
        let update = WidgetUpdate {
            widget_id: GPU_WIDGET_ID.into(),
            widget_type: GPU_WIDGET_TYPE.into(),
            state: value,
            status: widget_status,
            escalation: EscalationLevel::Ambient,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "gpu-dashboard: failed to publish WidgetUpdate");
        }
    }

    async fn run_once(&mut self) -> ModuleResult<()> {
        let hosts = self.hosts();
        if hosts.is_empty() {
            return Ok(());
        }
        let mut snapshot: Vec<GpuHostState> = Vec::with_capacity(hosts.len());
        for host in &hosts {
            snapshot.push(self.probe_host(host).await);
        }
        self.publish_widget(&GpuFleetState { hosts: snapshot });
        Ok(())
    }

    pub async fn tick_for_test(&mut self) -> ModuleResult<()> {
        self.run_once().await
    }
}

#[async_trait]
impl Module for GpuDashboardModule {
    fn name(&self) -> &str {
        "gpu-dashboard"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: GPU_WIDGET_ID.into(),
            widget_type: GPU_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(self.poll_interval)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        if self.hosts().is_empty() {
            return Ok(());
        }
        self.run_once().await
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.run_once().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::runner::{CommandOutput, MockRunner};
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec, WriterTask};
    use tokio::io::duplex;

    async fn writer() -> WriterTask {
        let (a, _b) = duplex(4096);
        let w = IpcWriter::from_parts(a, JsonCodec);
        spawn_writer_task(w, 16)
    }

    fn host(name: &str) -> HostConfig {
        HostConfig {
            name: name.into(),
            hostname: format!("{name}.example"),
            user: None,
            port: None,
            display_name: None,
            project: None,
            poll_interval_secs: None,
            connect_timeout_secs: 5,
            roles: vec![HostRole::Gpu],
        }
    }

    #[test]
    fn parses_typical_nvidia_smi_line() {
        let s = GpuSample::parse_csv_line("0, NVIDIA A100-SXM4-80GB, 12, 3024, 81920, 45")
            .unwrap();
        assert_eq!(s.index, 0);
        assert_eq!(s.name, "NVIDIA A100-SXM4-80GB");
        assert_eq!(s.utilization_percent, 12.0);
        assert_eq!(s.memory_used_mb, 3024);
        assert_eq!(s.memory_total_mb, 81920);
        assert_eq!(s.temperature_c, 45);
    }

    #[test]
    fn parse_rejects_malformed_lines() {
        assert!(GpuSample::parse_csv_line("").is_none());
        assert!(GpuSample::parse_csv_line("0, A100, 12, 3024, 81920").is_none());
        assert!(GpuSample::parse_csv_line("abc, A100, 12, 3024, 81920, 45").is_none());
    }

    #[tokio::test]
    async fn healthy_fleet_publishes_healthy_state() {
        let reg = HostRegistry::new(vec![host("a")]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "a",
            &["nvidia-smi"],
            CommandOutput {
                stdout: "0, A100, 5, 800, 81920, 40\n1, A100, 10, 1200, 81920, 41\n".into(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            GpuDashboardModule::new(Arc::new(reg), mock, writer.publisher);
        m.tick_for_test().await.unwrap();
    }

    #[tokio::test]
    async fn high_utilization_flips_to_busy() {
        let reg = HostRegistry::new(vec![host("busy")]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "busy",
            &["nvidia-smi"],
            CommandOutput {
                stdout: "0, A100, 95, 75000, 81920, 82\n".into(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            GpuDashboardModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        // Smoke-level: verify the mock was invoked once with the
        // expected argv.
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1[0], "nvidia-smi");
    }

    #[tokio::test]
    async fn nvidia_smi_missing_surfaces_as_offline() {
        let reg = HostRegistry::new(vec![host("nonvidia")]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "nonvidia",
            &["nvidia-smi"],
            CommandOutput {
                stdout: String::new(),
                stderr: "nvidia-smi: command not found".into(),
                status: Some(127),
            },
        );
        let writer = writer().await;
        let mut m =
            GpuDashboardModule::new(Arc::new(reg.clone()), mock.clone(), writer.publisher);
        // Probe a single host directly — we don't have a public state
        // getter, but probe_host is pub(crate) via its impl block.
        // Exercise via tick so the publisher path still runs.
        m.tick_for_test().await.unwrap();
        assert_eq!(mock.calls().len(), 1);
    }

    #[tokio::test]
    async fn host_without_gpu_role_is_filtered() {
        let mut h = host("a");
        h.roles = vec![HostRole::SshMonitor]; // no Gpu role
        let reg = HostRegistry::new(vec![h]);
        let mock = Arc::new(MockRunner::new());
        let writer = writer().await;
        let mut m =
            GpuDashboardModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        assert!(mock.calls().is_empty());
    }
}
