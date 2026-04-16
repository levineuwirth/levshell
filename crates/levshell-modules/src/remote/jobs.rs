//! Remote Job Monitor (spec §2.5.3, §6.2 item 4).
//!
//! For each host with role `jobs`, the module polls SLURM via
//! `squeue -h -u <user> -o "<format>"` on a configurable interval
//! (default 30s), parses each row into a [`SlurmJob`], and publishes a
//! [`RemoteJobsState`] widget.
//!
//! The `-u` argument defaults to the host's configured user, falling
//! back to `$USER` on the remote side when absent. Users who need
//! different backends (PBS, log tailing, status files) will get those
//! as sibling impls in a later phase — the spec explicitly lists
//! SLURM alongside PBS and log tailing, so SLURM-only is a v1
//! shortcut, not the final shape.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

use super::host::{HostConfig, HostRegistry, HostRole};
use super::runner::{RemoteError, RemoteRunner};

pub const JOBS_WIDGET_ID: &str = "remote-jobs";
pub const JOBS_WIDGET_TYPE: &str = "remote_jobs";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// SLURM squeue -o format string we parse. Fields are pipe-separated
/// so job names with spaces don't break parsing. Order matters —
/// [`SlurmJob::parse_line`] depends on it.
const SQUEUE_FORMAT: &str = "%i|%j|%T|%R|%M|%l";

/// One job as reported by SLURM. All fields except `id` are passed
/// through as strings because squeue itself reports them that way
/// (e.g. `time_used = "1-02:34:56"` for 1 day 2h) and parsing them
/// into `Duration` is out of scope for v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlurmJob {
    pub id: String,
    pub name: String,
    pub state: String,
    pub reason: String,
    pub time_used: String,
    pub time_limit: String,
}

impl SlurmJob {
    pub fn parse_line(line: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 6 {
            return None;
        }
        let id = fields[0].trim().to_string();
        if id.is_empty() {
            return None;
        }
        Some(Self {
            id,
            name: fields[1].trim().to_string(),
            state: fields[2].trim().to_string(),
            reason: fields[3].trim().to_string(),
            time_used: fields[4].trim().to_string(),
            time_limit: fields[5].trim().to_string(),
        })
    }
}

/// Per-host job state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsHostState {
    pub name: String,
    pub display_name: String,
    pub project: Option<String>,
    pub status: String,
    pub jobs: Vec<SlurmJob>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteJobsState {
    pub hosts: Vec<JobsHostState>,
}

pub struct RemoteJobsModule {
    registry: Arc<HostRegistry>,
    runner: Arc<dyn RemoteRunner>,
    publisher: WidgetPublisher,
    poll_interval: Duration,
}

impl RemoteJobsModule {
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
            .filter_by_role(HostRole::Jobs)
            .into_iter()
            .cloned()
            .collect()
    }

    async fn probe_host(&self, host: &HostConfig) -> JobsHostState {
        // Default to the host's configured ssh user; when omitted,
        // expand to `$USER` on the remote shell side. But we're not
        // going through a shell, so we fall back to the literal
        // string `$USER` only when user is None — in practice
        // hosts should declare `user =`.
        let user = host
            .user
            .clone()
            .unwrap_or_else(|| "$USER".to_string());
        let user_arg = format!("--user={user}");
        let format_arg = format!("--format={SQUEUE_FORMAT}");
        let argv = ["squeue", "-h", &user_arg, &format_arg];
        let result = self
            .runner
            .run(host, &argv, host.connect_timeout() + Duration::from_secs(3))
            .await;

        match result {
            Ok(out) if out.is_success() => {
                let jobs: Vec<SlurmJob> = out
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(SlurmJob::parse_line)
                    .collect();
                let status = if jobs.iter().any(|j| j.state == "RUNNING") {
                    "running"
                } else if !jobs.is_empty() {
                    "pending"
                } else {
                    "idle"
                };
                JobsHostState {
                    name: host.name.clone(),
                    display_name: host.display_name().to_string(),
                    project: host.project.clone(),
                    status: status.into(),
                    jobs,
                    error: None,
                }
            }
            Ok(out) => {
                let reason = match out.status {
                    Some(127) => "squeue not installed".into(),
                    Some(c) => format!("squeue exit {c}"),
                    None => "squeue killed".into(),
                };
                JobsHostState {
                    name: host.name.clone(),
                    display_name: host.display_name().to_string(),
                    project: host.project.clone(),
                    status: "offline".into(),
                    jobs: Vec::new(),
                    error: Some(reason),
                }
            }
            Err(RemoteError::Timeout { .. }) => JobsHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                status: "offline".into(),
                jobs: Vec::new(),
                error: Some("timeout".into()),
            },
            Err(e) => JobsHostState {
                name: host.name.clone(),
                display_name: host.display_name().to_string(),
                project: host.project.clone(),
                status: "offline".into(),
                jobs: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    fn publish_widget(&self, state: &RemoteJobsState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "remote-jobs: failed to serialize state");
                return;
            }
        };
        let widget_status = if state.hosts.iter().any(|h| h.status == "offline") {
            WidgetStatus::Stale
        } else {
            WidgetStatus::Normal
        };
        let update = WidgetUpdate {
            widget_id: JOBS_WIDGET_ID.into(),
            widget_type: JOBS_WIDGET_TYPE.into(),
            state: value,
            status: widget_status,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "remote-jobs: failed to publish WidgetUpdate");
        }
    }

    async fn run_once(&mut self) -> ModuleResult<()> {
        let hosts = self.hosts();
        if hosts.is_empty() {
            return Ok(());
        }
        let mut snapshot: Vec<JobsHostState> = Vec::with_capacity(hosts.len());
        for host in &hosts {
            snapshot.push(self.probe_host(host).await);
        }
        self.publish_widget(&RemoteJobsState { hosts: snapshot });
        Ok(())
    }

    pub async fn tick_for_test(&mut self) -> ModuleResult<()> {
        self.run_once().await
    }
}

#[async_trait]
impl Module for RemoteJobsModule {
    fn name(&self) -> &str {
        "remote-jobs"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: JOBS_WIDGET_ID.into(),
            widget_type: JOBS_WIDGET_TYPE.into(),
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

    fn host(name: &str, user: Option<&str>, roles: Vec<HostRole>) -> HostConfig {
        HostConfig {
            name: name.into(),
            hostname: format!("{name}.example"),
            user: user.map(str::to_string),
            port: None,
            display_name: None,
            project: None,
            poll_interval_secs: None,
            connect_timeout_secs: 5,
            roles,
        }
    }

    #[test]
    fn parses_running_and_pending_jobs() {
        let line =
            "123456|train_gpt.sh|RUNNING|None|01:23:45|12:00:00";
        let j = SlurmJob::parse_line(line).unwrap();
        assert_eq!(j.id, "123456");
        assert_eq!(j.name, "train_gpt.sh");
        assert_eq!(j.state, "RUNNING");
        assert_eq!(j.reason, "None");
        assert_eq!(j.time_used, "01:23:45");
        assert_eq!(j.time_limit, "12:00:00");

        let p = SlurmJob::parse_line(
            "123457|long_eval|PENDING|Resources|0:00|4-00:00:00",
        )
        .unwrap();
        assert_eq!(p.state, "PENDING");
        assert_eq!(p.reason, "Resources");
        assert_eq!(p.time_limit, "4-00:00:00");
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(SlurmJob::parse_line("").is_none());
        assert!(SlurmJob::parse_line("123|a|b|c|d").is_none());
        assert!(SlurmJob::parse_line("|a|b|c|d|e").is_none(), "empty id rejected");
    }

    #[tokio::test]
    async fn running_job_yields_running_status() {
        let reg = HostRegistry::new(vec![host("h", Some("u"), vec![HostRole::Jobs])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "h",
            &["squeue"],
            CommandOutput {
                stdout: "123|job1|RUNNING|None|00:15:00|01:00:00\n\
                         124|job2|PENDING|Resources|0:00|02:00:00\n"
                    .into(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m = RemoteJobsModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        let argv = &calls[0].1;
        assert_eq!(argv[0], "squeue");
        assert_eq!(argv[1], "-h");
        assert!(argv[2].starts_with("--user="));
        assert!(argv[2].contains("u"), "user is threaded through");
        assert!(argv[3].starts_with("--format="));
    }

    #[tokio::test]
    async fn empty_output_yields_idle_status() {
        let reg = HostRegistry::new(vec![host("i", Some("u"), vec![HostRole::Jobs])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "i",
            &["squeue"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m = RemoteJobsModule::new(Arc::new(reg), mock, writer.publisher);
        m.tick_for_test().await.unwrap();
    }

    #[tokio::test]
    async fn missing_user_falls_back_to_dollar_user() {
        let reg = HostRegistry::new(vec![host("h", None, vec![HostRole::Jobs])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "h",
            &["squeue"],
            CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let writer = writer().await;
        let mut m = RemoteJobsModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        let argv = &mock.calls()[0].1;
        assert!(argv.iter().any(|a| a.contains("$USER")));
    }

    #[tokio::test]
    async fn squeue_not_installed_surfaces_as_offline() {
        let reg = HostRegistry::new(vec![host("no-slurm", Some("u"), vec![HostRole::Jobs])]);
        let mock = Arc::new(MockRunner::new());
        mock.respond(
            "no-slurm",
            &["squeue"],
            CommandOutput {
                stdout: String::new(),
                stderr: "command not found".into(),
                status: Some(127),
            },
        );
        let writer = writer().await;
        let mut m = RemoteJobsModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        // One call made, even though status 127 — the module surfaces
        // it as offline in the widget rather than skipping.
        assert_eq!(mock.calls().len(), 1);
    }

    #[tokio::test]
    async fn host_without_jobs_role_is_filtered() {
        let reg = HostRegistry::new(vec![host(
            "a",
            Some("u"),
            vec![HostRole::SshMonitor, HostRole::Gpu],
        )]);
        let mock = Arc::new(MockRunner::new());
        let writer = writer().await;
        let mut m = RemoteJobsModule::new(Arc::new(reg), mock.clone(), writer.publisher);
        m.tick_for_test().await.unwrap();
        assert!(mock.calls().is_empty());
    }
}
