//! Data model for a saved context snapshot (spec §2.12.2).
//!
//! Captures enough of the sway window tree + per-window process state
//! that a later [`restore`](super::restore::plan_restore) pass can (a)
//! move windows that already exist back to their saved workspaces and
//! (b) re-launch apps that aren't running by replaying their
//! `/proc/{pid}/cmdline`.
//!
//! What's deliberately **not** in here, per v1 scope (confirmed with
//! user 2026-04-17):
//!
//! - Terminal scrollback, editor buffer positions, browser tabs — these
//!   need per-app hooks; skip for now.
//! - Audio routing / PipeWire state — feasible later via the quick
//!   settings plumbing, but not in v1.
//! - Scratchpad windows (on `__i3_scratch`) — skipped at capture time.
//! - Floating window geometry — we record the floating *flag* but not
//!   coordinates; sway will place re-launched floating windows with
//!   the compositor's defaults.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A complete captured desktop state. Serialized to JSON under
/// `$XDG_STATE_HOME/levshell/contexts/{name}.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// User-supplied name. Doubles as the filename stem.
    pub name: String,

    /// Wall-clock time of capture. Used for "saved 3h ago" hints in ctl
    /// output and future overlays.
    pub captured_at: DateTime<Utc>,

    /// The workspace that was focused at capture time. Restore focuses
    /// this workspace last so the user lands back where they were.
    pub focused_workspace: Option<String>,

    /// Every captured window, in no particular order. The restore
    /// planner matches these 1:1 against windows present in the tree
    /// at restore time.
    pub windows: Vec<WindowSnapshot>,
}

/// One captured window. Identity is `(app_id, title)` — see
/// [`super::restore`] for the matching policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    /// Name of the workspace this window was on at capture time.
    pub workspace: String,

    /// Wayland app_id. For XWayland windows sway reports no `app_id` —
    /// we fall back to `window_properties.class` at capture time and
    /// store it here. Never empty when serialized (unknown windows
    /// are skipped at capture).
    pub app_id: String,

    /// Window title at capture time. May be empty.
    pub title: String,

    /// Process arguments read from `/proc/{pid}/cmdline` — a
    /// NUL-separated list, stored here as a `Vec<String>`. `None` when
    /// the pid wasn't readable (the process may have been short-lived,
    /// or the daemon lacked permission). Restore cannot re-launch a
    /// window without a cmdline.
    pub cmdline: Option<Vec<String>>,

    /// Whether the window was floating at capture time. Echoed into the
    /// restore plan for informational purposes only — v1 does not
    /// restore floating geometry.
    #[serde(default)]
    pub floating: bool,
}

impl WindowSnapshot {
    /// Human-readable identity, used for log / ctl summaries.
    pub fn identity(&self) -> String {
        if self.title.is_empty() {
            self.app_id.clone()
        } else {
            format!("{} — {}", self.app_id, self.title)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> ContextSnapshot {
        ContextSnapshot {
            name: "research".into(),
            captured_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 30, 0).unwrap(),
            focused_workspace: Some("3:code".into()),
            windows: vec![
                WindowSnapshot {
                    workspace: "3:code".into(),
                    app_id: "neovide".into(),
                    title: "draft.md".into(),
                    cmdline: Some(vec!["neovide".into(), "--fork".into(), "draft.md".into()]),
                    floating: false,
                },
                WindowSnapshot {
                    workspace: "4:docs".into(),
                    app_id: "firefox".into(),
                    title: "arXiv abs".into(),
                    cmdline: None,
                    floating: false,
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let snap = sample();
        let s = serde_json::to_string(&snap).unwrap();
        let back: ContextSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn floating_defaults_to_false_when_absent() {
        let body = r#"{
            "name": "x",
            "captured_at": "2026-04-17T10:00:00Z",
            "focused_workspace": null,
            "windows": [{
                "workspace": "1",
                "app_id": "foot",
                "title": "",
                "cmdline": null
            }]
        }"#;
        let snap: ContextSnapshot = serde_json::from_str(body).unwrap();
        assert!(!snap.windows[0].floating);
    }

    #[test]
    fn identity_falls_back_to_app_id_when_title_empty() {
        let w = WindowSnapshot {
            workspace: "1".into(),
            app_id: "foot".into(),
            title: String::new(),
            cmdline: None,
            floating: false,
        };
        assert_eq!(w.identity(), "foot");
    }
}
