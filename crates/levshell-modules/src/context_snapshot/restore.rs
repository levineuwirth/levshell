//! Pure restore planning (spec §2.12.2).
//!
//! Given a saved [`ContextSnapshot`] and the current sway tree,
//! [`plan_restore`] produces two action lists:
//!
//! - **moves**: `con_id` + `target_workspace` for every saved window
//!   that matches an already-running window.
//! - **launches**: `cmdline` + `target_workspace` for every saved
//!   window that has no live match and carries a cmdline.
//!
//! Windows that have neither a match nor a cmdline are dropped silently
//! — the summary surfaces the count so the user knows.
//!
//! ## Matching policy (confirmed 2026-04-17)
//!
//! For each saved window, pick **an unused** current window with
//! matching `app_id`. When multiple candidates remain, prefer the one
//! with the same title; fall back to any. "Unused" means we haven't
//! already consumed it for an earlier saved window — a 1-to-1
//! assignment keeps two PDF viewers from both claiming the same live
//! window.

use std::collections::HashSet;

use super::capture::window_from_node_for_restore;
use super::model::{ContextSnapshot, WindowSnapshot};
use swayipc_async::{Node, NodeType};

/// One entry in the restore plan for an existing window that must move
/// to a different workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveAction {
    pub con_id: i64,
    pub target_workspace: String,
    pub identity: String,
}

/// One entry in the restore plan for a saved window that has no live
/// match and needs to be re-launched via the saved cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAction {
    pub cmdline: Vec<String>,
    pub target_workspace: String,
    pub identity: String,
}

/// Output of [`plan_restore`]. Contains actions to take + counts for
/// windows that were skipped because they had no cmdline and no match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    pub moves: Vec<MoveAction>,
    pub launches: Vec<LaunchAction>,
    /// Saved windows that had no live match and no cmdline, so we
    /// can't recreate them. Reported so the ctl summary can say
    /// "skipped N unrestorable windows".
    pub skipped_unrestorable: u32,
    /// The workspace to focus after all moves and launches are
    /// applied. Mirrors `snapshot.focused_workspace`.
    pub focused_workspace: Option<String>,
}

/// Shape of a live sway window when planning a restore — flattened from
/// the tree so the planner doesn't need to do the tree walk itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWindow {
    pub con_id: i64,
    pub workspace: String,
    pub app_id: String,
    pub title: String,
}

/// Flatten a sway tree into a `Vec<LiveWindow>` (skipping scratchpad,
/// matching the same policy as capture). Exposed so the async
/// driver can do `get_tree() → flatten_live_windows(&tree) →
/// plan_restore(&snap, &live)` in three steps.
pub fn flatten_live_windows(tree: &Node) -> Vec<LiveWindow> {
    let mut out = Vec::new();
    walk(tree, None, &mut out);
    out
}

const SCRATCHPAD_WORKSPACE: &str = "__i3_scratch";

fn walk(node: &Node, workspace: Option<&str>, out: &mut Vec<LiveWindow>) {
    let mut active = workspace.map(str::to_owned);

    if node.node_type == NodeType::Workspace {
        let name = node.name.clone().unwrap_or_default();
        if name == SCRATCHPAD_WORKSPACE {
            return;
        }
        active = Some(name);
    } else if let Some(ws) = active.as_deref() {
        if let Some((app_id, title)) = window_from_node_for_restore(node) {
            out.push(LiveWindow {
                con_id: node.id,
                workspace: ws.to_owned(),
                app_id,
                title,
            });
        }
    }

    for child in &node.nodes {
        walk(child, active.as_deref(), out);
    }
    for child in &node.floating_nodes {
        walk(child, active.as_deref(), out);
    }
}

/// Compute the restore plan. Pure — no IO, no async. `live` is usually
/// built from the current tree via [`flatten_live_windows`].
pub fn plan_restore(snapshot: &ContextSnapshot, live: &[LiveWindow]) -> RestorePlan {
    let mut plan = RestorePlan {
        focused_workspace: snapshot.focused_workspace.clone(),
        ..RestorePlan::default()
    };
    let mut consumed: HashSet<i64> = HashSet::new();

    for saved in &snapshot.windows {
        match find_best_match(saved, live, &consumed) {
            Some(idx) => {
                let live_win = &live[idx];
                consumed.insert(live_win.con_id);
                if live_win.workspace != saved.workspace {
                    plan.moves.push(MoveAction {
                        con_id: live_win.con_id,
                        target_workspace: saved.workspace.clone(),
                        identity: saved.identity(),
                    });
                }
                // Else: already on correct workspace — no action.
            }
            None => match saved.cmdline.as_ref() {
                Some(cmdline) if !cmdline.is_empty() => {
                    plan.launches.push(LaunchAction {
                        cmdline: cmdline.clone(),
                        target_workspace: saved.workspace.clone(),
                        identity: saved.identity(),
                    });
                }
                _ => {
                    plan.skipped_unrestorable = plan.skipped_unrestorable.saturating_add(1);
                }
            },
        }
    }

    plan
}

/// Return the index of the best matching live window, or `None` when
/// no unused window has the right app_id. Policy: same app_id
/// mandatory; prefer same title; fall back to any.
fn find_best_match(
    saved: &WindowSnapshot,
    live: &[LiveWindow],
    consumed: &HashSet<i64>,
) -> Option<usize> {
    let mut exact_title = None;
    let mut same_ws = None;
    let mut any = None;
    for (i, w) in live.iter().enumerate() {
        if consumed.contains(&w.con_id) {
            continue;
        }
        if w.app_id != saved.app_id {
            continue;
        }
        if w.title == saved.title {
            exact_title = Some(i);
            break;
        }
        // No title match yet — among ambiguous same-app candidates,
        // prefer one already on the saved workspace. This makes the
        // plan a no-op for an unchanged desktop (was: "first unused in
        // tree order", which churned and could swap two distinct
        // same-app windows onto each other's workspaces). Exact title
        // still wins outright — a stable unique title is the strongest
        // identity signal and should follow the window across spaces.
        if same_ws.is_none() && w.workspace == saved.workspace {
            same_ws = Some(i);
        }
        if any.is_none() {
            any = Some(i);
        }
    }
    exact_title.or(same_ws).or(any)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn saved(app: &str, title: &str, ws: &str, cmd: Option<&[&str]>) -> WindowSnapshot {
        WindowSnapshot {
            workspace: ws.into(),
            app_id: app.into(),
            title: title.into(),
            cmdline: cmd.map(|c| c.iter().map(|s| (*s).to_owned()).collect()),
            floating: false,
        }
    }

    fn live(id: i64, app: &str, title: &str, ws: &str) -> LiveWindow {
        LiveWindow {
            con_id: id,
            workspace: ws.into(),
            app_id: app.into(),
            title: title.into(),
        }
    }

    fn snapshot(windows: Vec<WindowSnapshot>) -> ContextSnapshot {
        ContextSnapshot {
            name: "t".into(),
            captured_at: Utc::now(),
            focused_workspace: None,
            windows,
        }
    }

    #[test]
    fn existing_window_on_right_workspace_is_no_op() {
        let snap = snapshot(vec![saved("firefox", "arxiv", "3", None)]);
        let live = vec![live(10, "firefox", "arxiv", "3")];
        let plan = plan_restore(&snap, &live);
        assert!(plan.moves.is_empty());
        assert!(plan.launches.is_empty());
    }

    #[test]
    fn existing_window_on_wrong_workspace_is_moved() {
        let snap = snapshot(vec![saved("firefox", "arxiv", "3", None)]);
        let live = vec![live(10, "firefox", "arxiv", "1")];
        let plan = plan_restore(&snap, &live);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].con_id, 10);
        assert_eq!(plan.moves[0].target_workspace, "3");
        assert!(plan.launches.is_empty());
    }

    #[test]
    fn missing_window_with_cmdline_is_launched() {
        let snap = snapshot(vec![saved(
            "neovide",
            "draft.md",
            "2",
            Some(&["neovide", "draft.md"]),
        )]);
        let live = vec![];
        let plan = plan_restore(&snap, &live);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.launches.len(), 1);
        assert_eq!(plan.launches[0].cmdline, vec!["neovide", "draft.md"]);
        assert_eq!(plan.launches[0].target_workspace, "2");
    }

    #[test]
    fn missing_window_without_cmdline_is_counted_as_skipped() {
        let snap = snapshot(vec![saved("neovide", "draft.md", "2", None)]);
        let live = vec![];
        let plan = plan_restore(&snap, &live);
        assert!(plan.launches.is_empty());
        assert!(plan.moves.is_empty());
        assert_eq!(plan.skipped_unrestorable, 1);
    }

    #[test]
    fn prefers_same_title_when_multiple_candidates() {
        // Two firefox windows, one title matches, one doesn't. Saved
        // snapshot wants the title-match.
        let snap = snapshot(vec![saved("firefox", "arxiv abs", "2", None)]);
        let live = vec![
            live(10, "firefox", "news", "1"),
            live(20, "firefox", "arxiv abs", "1"),
        ];
        let plan = plan_restore(&snap, &live);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].con_id, 20);
    }

    #[test]
    fn consumes_matched_windows_to_avoid_double_claims() {
        // Two saved firefox windows, two live firefox windows. Even
        // though both live windows have the same app_id, each saved
        // window takes one unique live window.
        let snap = snapshot(vec![
            saved("firefox", "a", "2", None),
            saved("firefox", "b", "3", None),
        ]);
        let live = vec![
            live(10, "firefox", "a", "1"),
            live(20, "firefox", "b", "1"),
        ];
        let plan = plan_restore(&snap, &live);
        assert_eq!(plan.moves.len(), 2);
        let targets: std::collections::HashSet<_> =
            plan.moves.iter().map(|m| (m.con_id, m.target_workspace.clone())).collect();
        assert!(targets.contains(&(10, "2".into())));
        assert!(targets.contains(&(20, "3".into())));
    }

    #[test]
    fn second_saved_window_falls_back_to_launch_when_only_one_live() {
        let snap = snapshot(vec![
            saved("firefox", "a", "2", None),
            saved("firefox", "b", "3", Some(&["firefox", "--new-window"])),
        ]);
        let live = vec![live(10, "firefox", "a", "1")];
        let plan = plan_restore(&snap, &live);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].con_id, 10);
        assert_eq!(plan.launches.len(), 1);
        assert_eq!(plan.launches[0].target_workspace, "3");
    }

    #[test]
    fn focused_workspace_is_preserved() {
        let snap = ContextSnapshot {
            focused_workspace: Some("7:focus".into()),
            ..snapshot(vec![])
        };
        let plan = plan_restore(&snap, &[]);
        assert_eq!(plan.focused_workspace.as_deref(), Some("7:focus"));
    }

    #[test]
    fn different_app_id_is_never_matched() {
        // Saved = firefox, live = chromium. No match even with same title.
        let snap = snapshot(vec![saved("firefox", "arxiv", "2", None)]);
        let live = vec![live(10, "chromium", "arxiv", "2")];
        let plan = plan_restore(&snap, &live);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.skipped_unrestorable, 1);
    }

    #[test]
    fn same_app_drifted_titles_prefer_same_workspace_no_churn() {
        // The exact scenario the live verification hit: two fungible
        // `foot` windows whose titles have all drifted since capture
        // (no exact-title match). Each is already on its saved
        // workspace. Old policy ("first unused in tree order") matched
        // saved@ws2 to the live window on ws6 and vice-versa → two
        // pointless cross-moves (and would swap distinct terminals).
        // Same-workspace preference must make this a no-op.
        let snap = snapshot(vec![
            saved("foot", "old-a", "2", None),
            saved("foot", "old-b", "6", None),
        ]);
        // Tree order puts the ws6 window first — this is what made the
        // old first-unused fallback churn.
        let live = vec![
            live(10, "foot", "new-y", "6"),
            live(11, "foot", "new-x", "2"),
        ];
        let plan = plan_restore(&snap, &live);
        assert!(
            plan.moves.is_empty(),
            "expected no moves, got {:?}",
            plan.moves
        );
        assert!(plan.launches.is_empty());
    }

    #[test]
    fn exact_title_still_wins_over_same_workspace() {
        // A stable unique title is the strongest identity signal: a
        // window that kept its title must follow it across workspaces
        // even though a same-app window already sits on the target.
        let snap = snapshot(vec![saved("foot", "build-log", "3", None)]);
        let live = vec![
            live(20, "foot", "scratch", "3"),   // same ws, wrong window
            live(21, "foot", "build-log", "5"), // the real one, elsewhere
        ];
        let plan = plan_restore(&snap, &live);
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].con_id, 21);
        assert_eq!(plan.moves[0].target_workspace, "3");
    }
}
