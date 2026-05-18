//! LaTeX compilation status (spec §2.9.10 — "When a LaTeX build is
//! running, a progress/status indicator appears in the bar (compiling →
//! success/error). Click to view the log on error").
//!
//! No external tooling and no editor integration: it watches `/proc`
//! for a TeX compiler process (the same hand-rolled scan the process
//! sniper uses), remembers that process's working directory, and when
//! the process exits inspects the newest `*.log` there for a LaTeX
//! error line (`^!`). "Quiet until important" (§1.3): the widget
//! collapses to nothing when idle.
//!
//! State: `{ phase: "idle"|"compiling"|"success"|"error",
//! log_path, error }`.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};

pub const LATEX_WIDGET_ID: &str = "latex-status";
pub const LATEX_WIDGET_TYPE: &str = "latex_status";
const MODULE_NAME: &str = "latex-status";

const TICK: StdDuration = StdDuration::from_secs(3);
/// TeX engine `comm` names worth reacting to.
const ENGINES: &[&str] = &[
    "pdflatex", "xelatex", "lualatex", "latex", "latexmk", "tectonic",
];
/// Ticks a success/error result stays visible before collapsing back to
/// idle (≈30s at the 3s tick) — long enough to read, short enough to
/// stay calm.
const RESULT_TICKS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Compiling,
    Success,
    Error,
}

impl Phase {
    fn wire(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Compiling => "compiling",
            Phase::Success => "success",
            Phase::Error => "error",
        }
    }
}

pub struct LatexStatusModule {
    publisher: WidgetPublisher,
    phase: Phase,
    /// `(pid, working-dir)` of the build we're currently watching.
    tracked: Option<(i32, PathBuf)>,
    log_path: Option<PathBuf>,
    error: Option<String>,
    /// Counts down while a result is shown; at zero we return to idle.
    result_ttl: u32,
}

impl LatexStatusModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            phase: Phase::Idle,
            tracked: None,
            log_path: None,
            error: None,
            result_ttl: 0,
        }
    }

    fn proc_comm(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    }

    /// First running TeX engine: `(pid, cwd)`. `cwd` resolves the
    /// `/proc/<pid>/cwd` symlink so we know where the `.log` lands.
    fn find_engine() -> Option<(i32, PathBuf)> {
        let rd = std::fs::read_dir("/proc").ok()?;
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            // Read `comm` exactly once per PID (was re-read per engine
            // name → up to ENGINES.len() syscalls per process per tick).
            let comm = Self::proc_comm(pid);
            if ENGINES.iter().any(|e| comm == *e) {
                let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
                    .unwrap_or_else(|_| PathBuf::from("."));
                return Some((pid, cwd));
            }
        }
        None
    }

    /// Newest `*.log` in `dir`, if any.
    fn newest_log(dir: &PathBuf) -> Option<PathBuf> {
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("log") {
                continue;
            }
            let Ok(m) = e.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if best.as_ref().map(|(t, _)| m > *t).unwrap_or(true) {
                best = Some((m, p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// First LaTeX error line (`! …`) in `log`, trimmed for the tooltip.
    fn first_error(log: &PathBuf) -> Option<String> {
        let text = std::fs::read_to_string(log).ok()?;
        text.lines()
            .find(|l| l.starts_with("! "))
            .map(|l| l.trim().chars().take(160).collect())
    }

    fn publish(&self) {
        let update = WidgetUpdate {
            widget_id: LATEX_WIDGET_ID.into(),
            widget_type: LATEX_WIDGET_TYPE.into(),
            state: serde_json::json!({
                "phase": self.phase.wire(),
                "log_path": self.log_path.as_ref().map(|p| p.display().to_string()),
                "error": self.error,
            }),
            status: WidgetStatus::Normal,
            escalation: Default::default(),
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "latex-status: publish drop");
        }
    }

    /// Blocking `/proc` + log scan. Runs on a `spawn_blocking` thread so
    /// the synchronous `std::fs` walk (a full `/proc` enumeration plus a
    /// `comm` read per process every tick, and a possibly-large `.log`
    /// read) never stalls a tokio runtime worker. `prev` is the tracked
    /// engine at tick start; if no engine is running now but one was, the
    /// just-finished build's newest log is inspected here too.
    fn probe(prev: Option<(i32, PathBuf)>) -> Probe {
        match Self::find_engine() {
            Some(e) => Probe {
                engine: Some(e),
                finished: None,
            },
            None => {
                let finished = prev.map(|(_, cwd)| {
                    let log = Self::newest_log(&cwd);
                    let error = log.as_ref().and_then(Self::first_error);
                    (log, error)
                });
                Probe {
                    engine: None,
                    finished,
                }
            }
        }
    }

    /// Apply a [`Probe`] to the module state. Pure (no I/O) — the
    /// blocking work already happened in [`Self::probe`].
    fn apply(&mut self, probe: Probe) {
        match probe.engine {
            Some((pid, cwd)) => {
                let already = self.tracked.as_ref().map(|(p, _)| *p) == Some(pid)
                    || self.phase == Phase::Compiling;
                self.tracked = Some((pid, cwd));
                if !already {
                    self.phase = Phase::Compiling;
                    self.error = None;
                    self.log_path = None;
                    self.publish();
                }
            }
            None => {
                if self.tracked.take().is_some() {
                    // Build just finished — use the probe's log inspection.
                    let (log, error) = probe.finished.unwrap_or((None, None));
                    self.error = error;
                    self.phase = if self.error.is_some() {
                        Phase::Error
                    } else {
                        Phase::Success
                    };
                    self.log_path = log;
                    self.result_ttl = RESULT_TICKS;
                    self.publish();
                } else if matches!(self.phase, Phase::Success | Phase::Error) {
                    if self.result_ttl > 0 {
                        self.result_ttl -= 1;
                    }
                    if self.result_ttl == 0 {
                        self.phase = Phase::Idle;
                        self.error = None;
                        self.log_path = None;
                        self.publish();
                    }
                }
            }
        }
    }
}

/// Result of the blocking `/proc` probe handed back to the async tick.
#[derive(Default)]
struct Probe {
    /// A TeX engine currently running: `(pid, cwd)`.
    engine: Option<(i32, PathBuf)>,
    /// If no engine runs now but one was tracked, the finished build's
    /// `(newest_log, first_error)`.
    finished: Option<(Option<PathBuf>, Option<String>)>,
}

#[async_trait]
impl Module for LatexStatusModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: LATEX_WIDGET_ID.into(),
            widget_type: LATEX_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(TICK)
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WidgetActionReceived]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        self.publish();
        Ok(())
    }

    /// Click-on-error opens the log (spec §2.9.10).
    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if let Event::WidgetActionReceived {
            widget_id, action, ..
        } = event
        {
            if widget_id == LATEX_WIDGET_ID && action == "open_log" {
                if let Some(path) = self.log_path.as_ref() {
                    let p = path.display().to_string();
                    if let Err(e) = crate::palette::spawn_detached("xdg-open", &[&p]) {
                        tracing::warn!(error = %e, "latex-status: open log failed");
                    }
                }
            }
        }
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        // Offload the synchronous /proc + log scan so it can't stall a
        // runtime worker (a join error → treat as a no-op tick).
        let prev = self.tracked.clone();
        let probe = tokio::task::spawn_blocking(move || Self::probe(prev))
            .await
            .unwrap_or_default();
        self.apply(probe);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn first_error_finds_bang_line() {
        let dir = std::env::temp_dir().join(format!("lvsh-latex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("paper.log");
        let mut f = std::fs::File::create(&log).unwrap();
        writeln!(f, "This is pdfTeX...\nLaTeX Warning: x\n! Undefined control sequence.\nl.42 \\foo")
            .unwrap();
        assert_eq!(
            LatexStatusModule::first_error(&log).as_deref(),
            Some("! Undefined control sequence.")
        );
        // A clean log → no error.
        let ok = dir.join("ok.log");
        std::fs::write(&ok, "Output written on ok.pdf (1 page).\n").unwrap();
        assert!(LatexStatusModule::first_error(&ok).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_log_picks_most_recent() {
        let dir = std::env::temp_dir().join(format!("lvsh-latex-n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.log"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("b.log"), "new").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let got = LatexStatusModule::newest_log(&dir).unwrap();
        assert_eq!(got.file_name().unwrap(), "b.log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn idle_when_no_engine_running() {
        use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
        use tokio::io::{duplex, AsyncReadExt};
        let (a, mut b) = duplex(4096);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 8);
        let mut m = LatexStatusModule::new(task.publisher);
        m.start().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = b.read(&mut buf).await.unwrap();
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(line.lines().next().unwrap()).unwrap();
        // No TeX engine in the test environment → idle.
        assert_eq!(v["state"]["phase"], "idle");
    }
}
