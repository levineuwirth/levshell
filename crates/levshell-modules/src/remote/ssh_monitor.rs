//! SSH Connection Dashboard (spec §2.5.1, §6.2 item 4).
//!
//! For each host with role `ssh_monitor`, the module runs a cheap
//! reachability probe on a configurable interval (default 30s) and
//! publishes the result as a [`SshFleetState`] widget.
//!
//! The probe is `ssh host true` — SSH's own session-establishment cost
//! dominates, so this is really a "can you reach this host on port 22,
//! with the user's auth config" test. Latency is the wall-clock
//! duration of the call.
//!
//! On any state transition (unreachable → reachable, or reachable →
//! unreachable) the module emits a [`levshell_core::Event::SshHostStatus`]
//! so downstream consumers (ideation engine, notification renderer)
//! can react.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate};

use crate::escalation::EscalationTracker;

/// Derive a raw escalation level from the published fleet state.
/// `Ambient` when every host is reachable; `Attention` once any host
/// drops; `Critical` only on a fleet-wide outage (all hosts down —
/// spec's "SSH tunnel died during active use" stand-in, since a
/// single-host fleet's sole host going dark is effectively that).
pub fn ssh_raw_escalation(state: &SshFleetState) -> EscalationLevel {
    if state.hosts.is_empty() {
        return EscalationLevel::Ambient;
    }
    let down = state.hosts.iter().filter(|h| !h.reachable).count();
    if down == 0 {
        EscalationLevel::Ambient
    } else if down == state.hosts.len() {
        EscalationLevel::Critical
    } else {
        EscalationLevel::Attention
    }
}
use serde::{Deserialize, Serialize};

use super::host::{HostConfig, HostRegistry, HostRole};
use super::runner::{RemoteError, RemoteRunner};

pub const SSH_WIDGET_ID: &str = "ssh-fleet";
pub const SSH_WIDGET_TYPE: &str = "ssh_fleet";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Latency above this counts as "degraded" — still reachable, but the
/// bar widget should show a yellow indicator rather than green. Spec
/// §2.5.1 mentions "color-coded latency indicator (green/yellow/red)".
const DEGRADED_LATENCY_MS: u32 = 250;

/// Per-host snapshot carried in the widget state and the bus event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshHostState {
    pub name: String,
    pub display_name: String,
    pub project: Option<String>,
    pub reachable: bool,
    pub latency_ms: Option<u32>,
    /// `"healthy"` / `"degraded"` / `"offline"`. Stringly-typed
    /// because the consumer is QML and the set is not user-extensible.
    pub status: String,
    /// Short human-readable reason when `reachable == false`, e.g.
    /// `"timeout"` or `"ssh exit 255"`.
    #[serde(default)]
    pub error: Option<String>,
}

/// The widget state — a stable-ordered vector of per-host snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshFleetState {
    pub hosts: Vec<SshHostState>,
}

pub struct SshMonitorModule {
    registry: Arc<HostRegistry>,
    runner: Arc<dyn RemoteRunner>,
    publisher: WidgetPublisher,
    bus: EventBus,
    /// Last-published reachability per host — compared on each tick
    /// so we only publish `SshHostStatus` on transitions.
    last_reachable: HashMap<String, bool>,
    poll_interval: Duration,
    escalation: EscalationTracker,
}

impl SshMonitorModule {
    pub fn new(
        registry: Arc<HostRegistry>,
        runner: Arc<dyn RemoteRunner>,
        publisher: WidgetPublisher,
        bus: EventBus,
    ) -> Self {
        Self {
            registry,
            runner,
            publisher,
            bus,
            last_reachable: HashMap::new(),
            poll_interval: DEFAULT_POLL_INTERVAL,
            escalation: EscalationTracker::new(),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    fn hosts(&self) -> Vec<HostConfig> {
        self.registry
            .filter_by_role(HostRole::SshMonitor)
            .into_iter()
            .cloned()
            .collect()
    }

    async fn probe_host(&self, host: &HostConfig) -> SshHostState {
        let started = Instant::now();
        let timeout = host.connect_timeout() + Duration::from_secs(1);
        let result = self.runner.run(host, &["true"], timeout).await;
        let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;

        match result {
            Ok(out) if out.is_success() => {
                let status = if elapsed_ms > DEGRADED_LATENCY_MS {
                    "degraded"
                } else {
                    "healthy"
                };
                SshHostState {
                    name: host.name.clone(),
                    display_name: host.display_name().to_string(),
                    project: host.project.clone(),
                    reachable: true,
                    latency_ms: Some(elapsed_ms),
                    status: status.into(),
                    error: None,
                }
            }
            Ok(out) => SshHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                reachable: false,
                latency_ms: None,
                status: "offline".into(),
                error: Some(format!(
                    "ssh exit {}",
                    out.status.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                )),
            },
            Err(RemoteError::Timeout { .. }) => SshHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                reachable: false,
                latency_ms: None,
                status: "offline".into(),
                error: Some("timeout".into()),
            },
            Err(e) => SshHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                reachable: false,
                latency_ms: None,
                status: "offline".into(),
                error: Some(e.to_string()),
            },
        }
    }

    fn publish_widget(&mut self, state: &SshFleetState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "ssh-monitor: failed to serialize state");
                return;
            }
        };
        let widget_status = if state.hosts.iter().any(|h| !h.reachable) {
            WidgetStatus::Stale
        } else {
            WidgetStatus::Normal
        };
        let outcome = self.escalation.step(ssh_raw_escalation(state));
        let update = WidgetUpdate {
            widget_id: SSH_WIDGET_ID.into(),
            widget_type: SSH_WIDGET_TYPE.into(),
            state: value,
            status: widget_status,
            escalation: outcome.level,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "ssh-monitor: failed to publish WidgetUpdate");
        }
        if outcome.entered_critical {
            let body = if state.hosts.len() == 1 {
                format!("SSH host {} unreachable", state.hosts[0].display_name)
            } else {
                format!("All {} SSH hosts unreachable", state.hosts.len())
            };
            self.bus.publish(Event::CriticalEscalation {
                widget_id: SSH_WIDGET_ID.into(),
                title: "SSH fleet down".into(),
                body,
            });
        }
    }

    async fn run_once(&mut self) -> ModuleResult<()> {
        let hosts = self.hosts();
        if hosts.is_empty() {
            return Ok(());
        }
        let mut snapshot: Vec<SshHostState> = Vec::with_capacity(hosts.len());
        for host in &hosts {
            let state = self.probe_host(host).await;

            let prev = self.last_reachable.insert(host.name.clone(), state.reachable);
            if prev != Some(state.reachable) {
                self.bus.publish(Event::SshHostStatus {
                    host: host.name.clone(),
                    reachable: state.reachable,
                    latency_ms: state.latency_ms,
                });
            }
            snapshot.push(state);
        }
        self.publish_widget(&SshFleetState { hosts: snapshot });
        Ok(())
    }

    /// Public seam for integration tests: drive exactly one probe
    /// pass without waiting for the tick timer.
    pub async fn tick_for_test(&mut self) -> ModuleResult<()> {
        self.run_once().await
    }
}

#[async_trait]
impl Module for SshMonitorModule {
    fn name(&self) -> &str {
        "ssh-monitor"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: SSH_WIDGET_ID.into(),
            widget_type: SSH_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(self.poll_interval)
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WidgetActionReceived]
    }

    /// One-click reconnect (spec §2.10.1). The daemon can't re-establish
    /// a user's SSH session, but it can immediately re-probe so a host
    /// that just came back clears its offline/stale state without waiting
    /// for the next 30s tick. `data.host` is logged but the whole fleet
    /// is re-probed — a single cheap pass that also refreshes neighbours.
    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if let Event::WidgetActionReceived {
            widget_id,
            action,
            data,
        } = event
        {
            if widget_id == SSH_WIDGET_ID && action == "reconnect" {
                let host = serde_json::from_str::<serde_json::Value>(data)
                    .ok()
                    .and_then(|v| v.get("host").and_then(|h| h.as_str()).map(str::to_owned));
                tracing::info!(
                    host = host.as_deref().unwrap_or("<all>"),
                    "ssh-monitor: reconnect requested — re-probing fleet"
                );
                self.run_once().await?;
            }
        }
        Ok(())
    }

    async fn start(&mut self) -> ModuleResult<()> {
        if self.hosts().is_empty() {
            // Dormant: declared host registry but no ssh_monitor
            // roles. Not an error — the other two modules may still
            // be doing useful work.
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

    fn host(name: &str, roles: Vec<HostRole>) -> HostConfig {
        HostConfig {
            name: name.into(),
            hostname: format!("{name}.example"),
            user: None,
            port: None,
            display_name: Some(format!("Display {name}")),
            project: Some("llm-alignment".into()),
            poll_interval_secs: None,
            connect_timeout_secs: 5,
            roles,
        }
    }

    async fn writer() -> WriterTask {
        let (a, _b) = duplex(4096);
        let w = IpcWriter::from_parts(a, JsonCodec);
        spawn_writer_task(w, 16)
    }

    fn fleet_state(reachability: &[bool]) -> SshFleetState {
        SshFleetState {
            hosts: reachability
                .iter()
                .enumerate()
                .map(|(i, r)| SshHostState {
                    name: format!("h{i}"),
                    display_name: format!("host-{i}"),
                    project: None,
                    reachable: *r,
                    latency_ms: if *r { Some(50) } else { None },
                    status: if *r { "healthy".into() } else { "offline".into() },
                    error: None,
                })
                .collect(),
        }
    }

    #[test]
    fn raw_escalation_empty_is_ambient() {
        assert_eq!(
            ssh_raw_escalation(&SshFleetState { hosts: vec![] }),
            EscalationLevel::Ambient
        );
    }

    #[test]
    fn raw_escalation_all_reachable_is_ambient() {
        assert_eq!(
            ssh_raw_escalation(&fleet_state(&[true, true, true])),
            EscalationLevel::Ambient
        );
    }

    #[test]
    fn raw_escalation_partial_outage_is_attention() {
        assert_eq!(
            ssh_raw_escalation(&fleet_state(&[true, false])),
            EscalationLevel::Attention
        );
    }

    #[test]
    fn raw_escalation_full_outage_is_critical() {
        assert_eq!(
            ssh_raw_escalation(&fleet_state(&[false, false])),
            EscalationLevel::Critical
        );
        assert_eq!(
            ssh_raw_escalation(&fleet_state(&[false])),
            EscalationLevel::Critical
        );
    }

    #[tokio::test]
    async fn no_registered_hosts_is_noop() {
        let bus = EventBus::new();
        let registry = Arc::new(HostRegistry::default());
        let runner = Arc::new(MockRunner::new());
        let writer = writer().await;
        let mut m = SshMonitorModule::new(registry, runner.clone(), writer.publisher, bus);
        m.tick_for_test().await.unwrap();
        assert!(runner.calls().is_empty(), "no hosts → no probes");
    }

    #[tokio::test]
    async fn healthy_host_publishes_healthy_status() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("t", [levshell_core::EventKind::SshHostStatus], 16);

        let reg = HostRegistry::new(vec![host("a", vec![HostRole::SshMonitor])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "a",
            &["true"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            SshMonitorModule::new(Arc::new(reg), mock.clone(), writer.publisher, bus.clone());
        m.tick_for_test().await.unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "a");
        assert_eq!(calls[0].1, vec!["true".to_string()]);

        let evt = rx.try_recv().expect("first probe emits transition event");
        match evt {
            Event::SshHostStatus {
                host,
                reachable,
                latency_ms,
            } => {
                assert_eq!(host, "a");
                assert!(reachable);
                assert!(latency_ms.is_some());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unreachable_host_fires_offline_event() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("t", [levshell_core::EventKind::SshHostStatus], 16);
        let reg = HostRegistry::new(vec![host("b", vec![HostRole::SshMonitor])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond_with_timeout("b", &["true"]);
        let writer = writer().await;
        let mut m =
            SshMonitorModule::new(Arc::new(reg), mock, writer.publisher, bus.clone());
        m.tick_for_test().await.unwrap();
        let evt = rx.try_recv().expect("event expected");
        match evt {
            Event::SshHostStatus {
                host, reachable, ..
            } => {
                assert_eq!(host, "b");
                assert!(!reachable);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn repeated_same_state_does_not_republish_event() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("t", [levshell_core::EventKind::SshHostStatus], 16);
        let reg = HostRegistry::new(vec![host("c", vec![HostRole::SshMonitor])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "c",
            &["true"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            SshMonitorModule::new(Arc::new(reg), mock, writer.publisher, bus.clone());
        m.tick_for_test().await.unwrap();
        let _first = rx.try_recv().expect("first tick emits");
        m.tick_for_test().await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "second healthy tick suppresses duplicate event"
        );
    }

    #[tokio::test]
    async fn filters_out_non_ssh_monitor_roles() {
        let bus = EventBus::new();
        let reg = HostRegistry::new(vec![
            host("a", vec![HostRole::SshMonitor]),
            host("b", vec![HostRole::Gpu]), // not observed by this module
        ]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "a",
            &["true"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            SshMonitorModule::new(Arc::new(reg), mock.clone(), writer.publisher, bus);
        m.tick_for_test().await.unwrap();
        let calls = mock.calls();
        let hosts: Vec<_> = calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(hosts, vec!["a"]);
    }

    #[tokio::test]
    async fn reconnect_action_triggers_immediate_reprobe() {
        let bus = EventBus::new();
        let reg = HostRegistry::new(vec![host("a", vec![HostRole::SshMonitor])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "a",
            &["true"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m =
            SshMonitorModule::new(Arc::new(reg), mock.clone(), writer.publisher, bus);

        // A reconnect for our widget re-probes immediately.
        m.on_event(&Event::WidgetActionReceived {
            widget_id: SSH_WIDGET_ID.into(),
            action: "reconnect".into(),
            data: r#"{"host":"a"}"#.into(),
        })
        .await
        .unwrap();
        assert_eq!(mock.calls().len(), 1, "reconnect should probe once");

        // An unrelated widget action is a no-op.
        m.on_event(&Event::WidgetActionReceived {
            widget_id: "some-other-widget".into(),
            action: "reconnect".into(),
            data: "{}".into(),
        })
        .await
        .unwrap();
        assert_eq!(mock.calls().len(), 1, "non-ssh action must not probe");
    }
}
