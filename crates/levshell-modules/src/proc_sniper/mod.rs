//! Process sniper (spec §2.3.5).
//!
//! Answers [`Event::ProcessListRequested`] (the user opened the CPU
//! widget's sniper) by sampling `/proc` twice ~120 ms apart, computing
//! a per-process CPU delta, and publishing the top few as
//! [`DaemonMessage::ProcessList`]. Handles [`Event::ProcessKillRequested`]
//! by sending the requested signal via `kill(1)`.
//!
//! No external deps: `/proc` is parsed by hand and the kill is a
//! detached `kill` child. Signals are allow-listed (TERM/KILL only) so
//! a malformed shell message can't deliver an arbitrary signal.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use levshell_core::{Event, EventKind, Module, ModuleResult};
use levshell_ipc::{DaemonMessage, ProcInfo, ProcessListPayload, WidgetPublisher};

const MODULE_NAME: &str = "proc-sniper";
/// How many rows the sniper shows.
const TOP_N: usize = 5;
/// Sampling gap for the CPU delta. Short enough to feel instant, long
/// enough to register a meaningful jiffy delta on busy processes.
const SAMPLE_GAP: Duration = Duration::from_millis(120);
/// USER_HZ. Effectively always 100 on Linux; libc sysconf isn't in std
/// and the percentage is explicitly approximate, so this is acceptable.
const CLK_TCK: f64 = 100.0;
/// Page size in KiB (4096 B on all targets we run on).
const PAGE_KB: u64 = 4;

pub struct ProcessSniperModule {
    publisher: WidgetPublisher,
    /// The active ranking ("cpu" | "mem"). Set on each
    /// `ProcessListRequested`; reused for the post-kill re-sample so
    /// the list stays ordered the way the user is looking at it.
    current_sort: String,
}

impl ProcessSniperModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            current_sort: "cpu".to_owned(),
        }
    }
}

/// (utime+stime) jiffies for `pid`, parsed from `/proc/<pid>/stat`.
/// Splits after the last `')'` so a comm containing spaces/parens
/// doesn't shift field offsets.
fn proc_jiffies(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = after.split_whitespace().collect();
    // Post-')' index 11 = utime, 12 = stime (stat fields 14/15).
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(utime + stime)
}

fn proc_name(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| format!("pid {pid}"))
}

fn proc_rss_kb(pid: i32) -> u64 {
    // /proc/<pid>/statm: size resident shared ... (pages)
    std::fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
        .map(|pages| pages * PAGE_KB)
        .unwrap_or(0)
}

fn list_pids() -> Vec<i32> {
    let mut pids = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            if let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) {
                pids.push(pid);
            }
        }
    }
    pids
}

impl ProcessSniperModule {
    async fn publish_top(&self, sort: &str) {
        let t0_pids = list_pids();
        let mut first: HashMap<i32, u64> = HashMap::with_capacity(t0_pids.len());
        for pid in &t0_pids {
            if let Some(j) = proc_jiffies(*pid) {
                first.insert(*pid, j);
            }
        }
        let start = Instant::now();
        tokio::time::sleep(SAMPLE_GAP).await;
        let dt = start.elapsed().as_secs_f64().max(0.001);

        let mut rows: Vec<ProcInfo> = Vec::new();
        for pid in list_pids() {
            let Some(j1) = proc_jiffies(pid) else { continue };
            let j0 = first.get(&pid).copied().unwrap_or(j1);
            let delta = j1.saturating_sub(j0);
            let cpu_percent = (delta as f64 / CLK_TCK) / dt * 100.0;
            rows.push(ProcInfo {
                pid,
                name: proc_name(pid),
                cpu_percent,
                mem_kb: proc_rss_kb(pid),
            });
        }
        if sort == "mem" {
            rows.sort_by_key(|p| std::cmp::Reverse(p.mem_kb));
        } else {
            rows.sort_by(|a, b| {
                b.cpu_percent
                    .partial_cmp(&a.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        rows.truncate(TOP_N);

        let payload = ProcessListPayload {
            generated_at: Utc::now().to_rfc3339(),
            sort: sort.to_owned(),
            processes: rows,
        };
        if let Err(e) = self
            .publisher
            .try_send(DaemonMessage::ProcessList(Box::new(payload)))
        {
            tracing::warn!(error = %e, "proc-sniper: publish drop");
        }
    }

    fn kill(pid: i32, signal: &str) {
        // Allow-list: the sniper only ever offers graceful/forceful.
        let sig = match signal {
            "KILL" | "SIGKILL" => "KILL",
            "TERM" | "SIGTERM" => "TERM",
            other => {
                tracing::warn!(signal = %other, "proc-sniper: refusing non-allowlisted signal");
                return;
            }
        };
        if pid <= 1 {
            tracing::warn!(pid, "proc-sniper: refusing to signal pid <= 1");
            return;
        }
        match std::process::Command::new("kill")
            .arg(format!("-{sig}"))
            .arg(pid.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => tracing::info!(pid, signal = %sig, "proc-sniper: signalled"),
            Err(e) => tracing::warn!(error = %e, pid, "proc-sniper: kill spawn failed"),
        }
    }
}

#[async_trait]
impl Module for ProcessSniperModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::ProcessListRequested,
            EventKind::ProcessKillRequested,
        ]
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::ProcessListRequested { sort } => {
                self.current_sort = sort.clone();
                self.publish_top(sort).await;
            }
            Event::ProcessKillRequested { pid, signal } => {
                Self::kill(*pid, signal);
                // Re-sample in the ranking the user is viewing.
                let sort = self.current_sort.clone();
                self.publish_top(&sort).await;
            }
            _ => {}
        }
        Ok(())
    }
}
