//! Disk-space warning module (spec §2.3.4).
//!
//! Calls `statvfs(2)` on each configured mount every tick and publishes a
//! per-mount usage list. The widget shows the tightest mount; the module
//! escalates (and fires a one-shot critical notification) when free space
//! crosses the §9 ambient → aware → attention → critical thresholds, so a
//! filling-up root partition surfaces before a write fails rather than
//! after.
//!
//! `statvfs` reaches the disk driver, not the platter, so a blocking call
//! is fine — same reasoning the other telemetry modules apply to procfs
//! (see [`super`]). Configured via `modules/disk.toml`; with no file the
//! module watches `/` alone, which is the case that actually breaks a
//! session when it fills.
//!
//! State: `{ mounts: [{ path, total_bytes, used_bytes, avail_bytes,
//! used_percent }] }`, sorted tightest-first.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use levshell_core::{
    Event, EventBus, Module, ModuleError, ModuleResult, WidgetDescriptor,
};
use levshell_ipc::{
    DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate,
};
use serde::{Deserialize, Serialize};

use crate::escalation::EscalationTracker;

pub const DISK_WIDGET_ID: &str = "disk";
pub const DISK_WIDGET_TYPE: &str = "disk";

const TICK_INTERVAL: Duration = Duration::from_secs(60);

/// Disk usage escalation, mirroring the §9 battery shape (a slow-moving
/// resource the user wants warned about well before it bites):
/// * Aware at ≥85 % used,
/// * Attention at ≥92 %,
/// * Critical at ≥97 % (a few hundred MB from a failed write on a
///   typical SSD).
pub fn disk_raw_escalation(used_percent: u8) -> EscalationLevel {
    if used_percent >= 97 {
        EscalationLevel::Critical
    } else if used_percent >= 92 {
        EscalationLevel::Attention
    } else if used_percent >= 85 {
        EscalationLevel::Aware
    } else {
        EscalationLevel::Ambient
    }
}

/// `~/.config/levshell/modules/disk.toml`. Absent → watch `/` only.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiskConfig {
    pub mounts: Vec<String>,
}

impl Default for DiskConfig {
    fn default() -> Self {
        Self {
            mounts: vec!["/".to_owned()],
        }
    }
}

impl DiskConfig {
    pub fn load_from_dir(dir: &Path) -> Self {
        let path = dir.join("disk.toml");
        match std::fs::read_to_string(&path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "disk.toml malformed; watching / only");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
        .normalized()
    }

    /// An empty `mounts` list (e.g. `mounts = []`) would silence the
    /// module entirely — almost certainly not what an explicit config
    /// file meant, so fall back to the safe default.
    fn normalized(mut self) -> Self {
        if self.mounts.is_empty() {
            self.mounts = Self::default().mounts;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountUsage {
    pub path: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub avail_bytes: u64,
    pub used_percent: u8,
}

/// Build a [`MountUsage`] from raw `statvfs` block counts. `avail` is the
/// space usable by an unprivileged process (`f_bavail`); root-reserved
/// blocks count as used here, which matches what `df` shows in its
/// `Use%` column and what a bar user expects to see.
///
/// `frsize` of 0 (impossible on a real mount, but guard anyway) yields a
/// zeroed entry rather than a divide-by-zero.
pub fn mount_usage(path: &str, blocks: u64, bavail: u64, frsize: u64) -> MountUsage {
    let total = blocks.saturating_mul(frsize);
    let avail = bavail.saturating_mul(frsize);
    let used = total.saturating_sub(avail);
    let used_percent = if total == 0 {
        0
    } else {
        ((used as u128 * 100) / total as u128) as u8
    };
    MountUsage {
        path: path.to_owned(),
        total_bytes: total,
        used_bytes: used,
        avail_bytes: avail,
        used_percent,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskState {
    /// Sorted tightest-first so the shell can render `mounts[0]` as the
    /// headline without re-sorting.
    pub mounts: Vec<MountUsage>,
}

pub struct DiskModule {
    bus: EventBus,
    publisher: WidgetPublisher,
    config: DiskConfig,
    escalation: EscalationTracker,
}

impl DiskModule {
    pub fn new(bus: EventBus, publisher: WidgetPublisher, config: DiskConfig) -> Self {
        Self {
            bus,
            publisher,
            config,
            escalation: EscalationTracker::new(),
        }
    }

    /// Sample every configured mount. A mount that can't be `statvfs`'d
    /// (unmounted, typo'd path) is logged and skipped rather than
    /// failing the whole module — the other mounts still matter.
    fn sample(&self) -> DiskState {
        let mut mounts = Vec::with_capacity(self.config.mounts.len());
        for m in &self.config.mounts {
            match rustix::fs::statvfs(m.as_str()) {
                Ok(s) => mounts.push(mount_usage(
                    m,
                    s.f_blocks,
                    s.f_bavail,
                    s.f_frsize,
                )),
                Err(e) => {
                    tracing::warn!(mount = %m, error = %e, "telemetry-disk: statvfs failed; skipping mount");
                }
            }
        }
        mounts.sort_by(|a, b| {
            b.used_percent
                .cmp(&a.used_percent)
                .then_with(|| a.path.cmp(&b.path))
        });
        DiskState { mounts }
    }

    fn publish(&mut self, state: &DiskState) {
        let value = match serde_json::to_value(state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry-disk: failed to serialize state");
                return;
            }
        };
        // Escalate on the tightest mount (sorted first).
        let worst = state.mounts.first();
        let raw = worst
            .map(|m| disk_raw_escalation(m.used_percent))
            .unwrap_or(EscalationLevel::Ambient);
        let outcome = self.escalation.step(raw);
        let update = WidgetUpdate {
            widget_id: DISK_WIDGET_ID.into(),
            widget_type: DISK_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: outcome.level,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "telemetry-disk: failed to publish WidgetUpdate");
        }
        if outcome.entered_critical {
            if let Some(m) = worst {
                self.bus.publish(Event::CriticalEscalation {
                    widget_id: DISK_WIDGET_ID.into(),
                    title: "Disk almost full".into(),
                    body: format!(
                        "{} is {}% full ({} free)",
                        m.path,
                        m.used_percent,
                        human_bytes(m.avail_bytes)
                    ),
                });
            }
        }
    }
}

/// Compact binary-prefix byte size for the critical-notification body
/// (the widget itself gets raw bytes and formats its own way).
fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[async_trait]
impl Module for DiskModule {
    fn name(&self) -> &str {
        "telemetry-disk"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: DISK_WIDGET_ID.into(),
            widget_type: DISK_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(TICK_INTERVAL)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        let state = self.sample();
        if state.mounts.is_empty() {
            return Err(ModuleError::Unavailable(
                "no configured disk mount could be stat'd".into(),
            ));
        }
        self.publish(&state);
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        let state = self.sample();
        self.publish(&state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_escalation_thresholds() {
        assert_eq!(disk_raw_escalation(0), EscalationLevel::Ambient);
        assert_eq!(disk_raw_escalation(84), EscalationLevel::Ambient);
        assert_eq!(disk_raw_escalation(85), EscalationLevel::Aware);
        assert_eq!(disk_raw_escalation(91), EscalationLevel::Aware);
        assert_eq!(disk_raw_escalation(92), EscalationLevel::Attention);
        assert_eq!(disk_raw_escalation(96), EscalationLevel::Attention);
        assert_eq!(disk_raw_escalation(97), EscalationLevel::Critical);
        assert_eq!(disk_raw_escalation(100), EscalationLevel::Critical);
    }

    #[test]
    fn mount_usage_computes_percent_and_bytes() {
        // 1000 blocks of 4 KiB = ~3.9 MiB total; 250 available → 75% used.
        let u = mount_usage("/", 1000, 250, 4096);
        assert_eq!(u.total_bytes, 4_096_000);
        assert_eq!(u.avail_bytes, 1_024_000);
        assert_eq!(u.used_bytes, 3_072_000);
        assert_eq!(u.used_percent, 75);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn mount_usage_full_disk_is_100_percent() {
        let u = mount_usage("/data", 500, 0, 4096);
        assert_eq!(u.used_percent, 100);
        assert_eq!(u.avail_bytes, 0);
    }

    #[test]
    fn mount_usage_guards_zero_frsize() {
        let u = mount_usage("/x", 1000, 100, 0);
        assert_eq!(u.total_bytes, 0);
        assert_eq!(u.used_percent, 0);
    }

    #[test]
    fn empty_mounts_config_falls_back_to_root() {
        let cfg = DiskConfig { mounts: vec![] }.normalized();
        assert_eq!(cfg.mounts, vec!["/".to_owned()]);
    }

    #[test]
    fn missing_config_dir_defaults_to_root() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DiskConfig::load_from_dir(dir.path());
        assert_eq!(cfg.mounts, vec!["/".to_owned()]);
    }

    #[test]
    fn config_parses_explicit_mounts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("disk.toml"),
            "mounts = [\"/\", \"/home\", \"/scratch\"]\n",
        )
        .unwrap();
        let cfg = DiskConfig::load_from_dir(dir.path());
        assert_eq!(cfg.mounts, vec!["/", "/home", "/scratch"]);
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
