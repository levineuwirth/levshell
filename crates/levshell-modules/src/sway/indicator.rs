//! Pure state-computation logic for the workspace indicator widget.
//!
//! This module deliberately does not depend on `swayipc-async`. It accepts a
//! plain `Vec<WorkspaceInfo>` and produces both the in-memory indicator state
//! and the serialized [`WidgetUpdate`] the daemon pushes to QML. Keeping the
//! conversion pure makes it trivial to unit-test without a running Sway and
//! lets the production [`super::module::SwayWorkspaceModule`] do nothing
//! more than translate `swayipc_async::Workspace` into our shape.

use levshell_ipc::{EscalationLevel, WidgetStatus, WidgetUpdate};
use serde::{Deserialize, Serialize};

/// The widget id and type the indicator is published under. Both the daemon
/// (here) and the QML side will reference these constants.
pub const WORKSPACE_WIDGET_ID: &str = "workspace-indicator";
pub const WORKSPACE_WIDGET_TYPE: &str = "workspace_indicator";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub num: i32,
    pub focused: bool,
    pub urgent: bool,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIndicatorState {
    pub workspaces: Vec<WorkspaceInfo>,
    pub active: Option<String>,
    pub focused_window: Option<String>,
}

impl WorkspaceIndicatorState {
    pub fn from_workspaces(workspaces: impl IntoIterator<Item = WorkspaceInfo>) -> Self {
        let workspaces: Vec<_> = workspaces.into_iter().collect();
        let active = workspaces
            .iter()
            .find(|w| w.focused)
            .map(|w| w.name.clone());
        Self {
            workspaces,
            active,
            focused_window: None,
        }
    }

    pub fn with_focused_window(mut self, title: Option<String>) -> Self {
        self.focused_window = title;
        self
    }

    /// Serialize this state into the [`WidgetUpdate`] payload pushed to QML.
    /// `serde_json::to_value` only fails if `Serialize` returns an error,
    /// which can't happen for a struct of `String`/`i32`/`Option<String>`.
    pub fn into_widget_update(self, status: WidgetStatus) -> WidgetUpdate {
        let state = serde_json::to_value(&self)
            .expect("WorkspaceIndicatorState always serializes");
        WidgetUpdate {
            widget_id: WORKSPACE_WIDGET_ID.into(),
            widget_type: WORKSPACE_WIDGET_TYPE.into(),
            state,
            status,
            escalation: EscalationLevel::Ambient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, num: i32, focused: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            name: name.into(),
            num,
            focused,
            urgent: false,
            output: "eDP-1".into(),
        }
    }

    #[test]
    fn from_workspaces_picks_focused_as_active() {
        let state = WorkspaceIndicatorState::from_workspaces([
            ws("research", 1, false),
            ws("writing", 2, true),
            ws("reading", 3, false),
        ]);
        assert_eq!(state.active.as_deref(), Some("writing"));
        assert_eq!(state.workspaces.len(), 3);
    }

    #[test]
    fn no_focused_workspace_means_no_active() {
        let state = WorkspaceIndicatorState::from_workspaces([ws("a", 1, false)]);
        assert!(state.active.is_none());
    }

    #[test]
    fn empty_workspaces_round_trips() {
        let state = WorkspaceIndicatorState::from_workspaces([]);
        assert!(state.workspaces.is_empty());
        assert!(state.active.is_none());
    }

    #[test]
    fn with_focused_window_attaches_title() {
        let state = WorkspaceIndicatorState::from_workspaces([ws("a", 1, true)])
            .with_focused_window(Some("Alacritty".into()));
        assert_eq!(state.focused_window.as_deref(), Some("Alacritty"));
    }

    #[test]
    fn into_widget_update_uses_canonical_ids_and_status() {
        let state = WorkspaceIndicatorState::from_workspaces([ws("a", 1, true)]);
        let update = state.into_widget_update(WidgetStatus::Normal);
        assert_eq!(update.widget_id, WORKSPACE_WIDGET_ID);
        assert_eq!(update.widget_type, WORKSPACE_WIDGET_TYPE);
        assert_eq!(update.status, WidgetStatus::Normal);
        let active = update.state.get("active").and_then(|v| v.as_str());
        assert_eq!(active, Some("a"));
    }
}
