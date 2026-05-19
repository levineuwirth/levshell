//! Capture a [`ContextSnapshot`] from a sway tree.
//!
//! The walk is pure — it takes a `swayipc_types::Node` plus a closure
//! for reading `/proc/{pid}/cmdline` and returns a snapshot. Splitting
//! the procfs read behind a closure keeps the core function testable
//! with arbitrary pid → cmdline mappings.
//!
//! ## Walk
//!
//! Sway's tree is `Root → Output → Workspace → (Con | FloatingCon)*`
//! with containers nested arbitrarily inside workspaces. We treat any
//! descendant of a workspace that has an `app_id` (Wayland) or a
//! `window_properties.class` (XWayland) as a captured window — split
//! containers and tab/stack containers have neither and are
//! transparently descended through.
//!
//! The special `__i3_scratch` workspace is skipped: scratchpad state
//! is ephemeral and the user hasn't asked for it in v1.

use std::path::Path;

use chrono::Utc;
use swayipc_async::{Node, NodeType};

use super::model::{ContextSnapshot, WindowSnapshot};

/// Extract `(app_id, title)` from a window-bearing node, or `None` if
/// the node isn't a window. Exposed so the restore planner can reuse
/// the same detection logic (Wayland app_id + XWayland class fallback)
/// when flattening the live tree.
pub fn window_from_node_for_restore(node: &Node) -> Option<(String, String)> {
    let app_id = node.app_id.clone().or_else(|| {
        node.window_properties
            .as_ref()
            .and_then(|wp| wp.class.clone())
    })?;
    let title = node.name.clone().unwrap_or_default();
    Some((app_id, title))
}

const SCRATCHPAD_WORKSPACE: &str = "__i3_scratch";

/// Read `/proc/{pid}/cmdline` and split it on NUL bytes. Returns `None`
/// when the file is missing / unreadable or empty. This is the default
/// cmdline reader used by [`capture_from_tree`].
pub fn read_cmdline_from_proc(pid: i32) -> Option<Vec<String>> {
    let path = Path::new("/proc").join(pid.to_string()).join("cmdline");
    let bytes = std::fs::read(&path).ok()?;
    parse_proc_cmdline(&bytes)
}

/// Parse a raw `/proc/{pid}/cmdline` blob. Exposed so tests can feed
/// synthetic bytes without hitting the filesystem.
pub fn parse_proc_cmdline(bytes: &[u8]) -> Option<Vec<String>> {
    // /proc/*/cmdline is NUL-separated and *usually* ends with a
    // trailing NUL. An empty file (some kernel threads) yields no args.
    let args: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|slice| !slice.is_empty())
        .map(|slice| String::from_utf8_lossy(slice).into_owned())
        .collect();
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

/// Pure capture. `read_cmdline` is invoked once per window that carries
/// a pid — tests pass a closure over a fake `HashMap<pid, Vec<String>>`.
pub fn capture_from_tree(
    name: &str,
    tree: &Node,
    read_cmdline: impl Fn(i32) -> Option<Vec<String>>,
) -> ContextSnapshot {
    let mut windows = Vec::new();
    let mut focused_workspace = None;
    walk(tree, None, &mut windows, &mut focused_workspace, &read_cmdline);
    ContextSnapshot {
        name: name.to_owned(),
        captured_at: Utc::now(),
        focused_workspace,
        windows,
    }
}

fn walk(
    node: &Node,
    workspace: Option<&str>,
    windows: &mut Vec<WindowSnapshot>,
    focused_workspace: &mut Option<String>,
    read_cmdline: &impl Fn(i32) -> Option<Vec<String>>,
) {
    let mut active_workspace = workspace.map(str::to_owned);

    if node.node_type == NodeType::Workspace {
        let ws_name = node.name.clone().unwrap_or_default();
        if ws_name == SCRATCHPAD_WORKSPACE {
            return; // skip scratchpad entirely
        }
        active_workspace = Some(ws_name);
    } else if let Some(ws) = active_workspace.as_deref() {
        // Inside a workspace — check whether this node is a window.
        if let Some(snap) = window_from_node(node, ws, read_cmdline) {
            windows.push(snap);
        }
    }

    // Record the focused workspace from whatever node carries focus.
    // Sway sets `focused: true` on the focused *window con*, not its
    // ancestor Workspace node (the workspace node is only `focused`
    // when an empty workspace itself holds focus). Keying off the
    // enclosing `active_workspace` of ANY focused node covers both
    // cases; the earlier Workspace-only check silently captured
    // `None` in the common "a window is focused" case.
    if node.focused && focused_workspace.is_none() {
        if let Some(ws) = active_workspace.as_deref() {
            *focused_workspace = Some(ws.to_owned());
        }
    }

    // Propagate the active workspace into both regular and floating
    // children. A floating window inside a workspace is still "on"
    // that workspace from the user's point of view.
    for child in &node.nodes {
        walk(child, active_workspace.as_deref(), windows, focused_workspace, read_cmdline);
    }
    for child in &node.floating_nodes {
        walk(child, active_workspace.as_deref(), windows, focused_workspace, read_cmdline);
    }

    // Capture the *focused workspace* as whatever Workspace node was
    // marked focused. Sway also marks a container as focused when a
    // window inside the workspace has focus; we want the workspace,
    // not the window, so we only read from NodeType::Workspace above.
    // (This assignment is above.)
}

fn window_from_node(
    node: &Node,
    workspace: &str,
    read_cmdline: &impl Fn(i32) -> Option<Vec<String>>,
) -> Option<WindowSnapshot> {
    // Wayland: app_id is set directly on the node.
    // XWayland: fall back to window_properties.class.
    let app_id = node.app_id.clone().or_else(|| {
        node.window_properties
            .as_ref()
            .and_then(|wp| wp.class.clone())
    })?;

    // Containers (splits, stacks, tabs) have no app_id/class, so we
    // never get here for them — good. Anything else that does carry an
    // app_id or class is a window we want to capture.

    let title = node.name.clone().unwrap_or_default();
    let cmdline = node.pid.and_then(read_cmdline);
    let floating = node.node_type == NodeType::FloatingCon;

    Some(WindowSnapshot {
        workspace: workspace.to_owned(),
        app_id,
        title,
        cmdline,
        floating,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// swayipc `Node` has many required fields with no `#[serde(default)]`
    /// (rect, geometry, focus list, etc.). `build_node` stamps out a
    /// minimal valid Node given a few shape inputs; tests compose full
    /// trees from it.
    fn build_node(
        id: i64,
        name: Option<&str>,
        node_type: &str,
        focused: bool,
        children: Vec<Value>,
        floating_children: Vec<Value>,
        extras: Value,
    ) -> Value {
        let zero_rect = json!({"x":0,"y":0,"width":0,"height":0});
        let mut base = json!({
            "id": id,
            "name": name,
            "type": node_type,
            "border": "none",
            "current_border_width": 0,
            "layout": "splith",
            "percent": null,
            "rect": zero_rect,
            "window_rect": zero_rect,
            "deco_rect": zero_rect,
            "geometry": zero_rect,
            "urgent": false,
            "focused": focused,
            "focus": [],
            "floating": null,
            "nodes": children,
            "floating_nodes": floating_children,
            "sticky": false,
            "representation": null,
            "fullscreen_mode": null,
            "scratchpad_state": null,
            "app_id": null,
            "pid": null,
            "window": null,
            "num": null,
            "window_properties": null,
            "marks": [],
            "inhibit_idle": null,
            "idle_inhibitors": null,
            "sandbox_engine": null,
            "sandbox_app_id": null,
            "sandbox_instance_id": null,
            "tag": null,
            "shell": null,
            "foreign_toplevel_identifier": null,
            "visible": null,
            "output": null
        });
        // Merge caller-supplied overrides onto the base.
        if let Value::Object(m) = extras {
            if let Value::Object(ref mut obj) = base {
                for (k, v) in m {
                    obj.insert(k, v);
                }
            }
        }
        base
    }

    fn con(id: i64, title: &str, app_id: &str, pid: Option<i32>) -> Value {
        build_node(
            id,
            Some(title),
            "con",
            false,
            vec![],
            vec![],
            json!({ "app_id": app_id, "pid": pid }),
        )
    }

    fn from_value(v: Value) -> Node {
        serde_json::from_value(v).expect("valid sway node json")
    }

    /// A tree with one workspace "3:code" containing a single Wayland
    /// window (neovide).
    fn tree_one_window() -> Node {
        let win = con(4, "draft.md", "neovide", Some(1234));
        let ws = build_node(
            3,
            Some("3:code"),
            "workspace",
            true,
            vec![win],
            vec![],
            json!({}),
        );
        let output = build_node(2, Some("HDMI-0"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        from_value(root)
    }

    #[test]
    fn captures_a_single_wayland_window() {
        let tree = tree_one_window();
        let snap = capture_from_tree("x", &tree, |pid| {
            assert_eq!(pid, 1234);
            Some(vec!["neovide".into(), "draft.md".into()])
        });
        assert_eq!(snap.windows.len(), 1);
        let w = &snap.windows[0];
        assert_eq!(w.workspace, "3:code");
        assert_eq!(w.app_id, "neovide");
        assert_eq!(w.title, "draft.md");
        assert_eq!(
            w.cmdline.as_deref(),
            Some(&["neovide".to_owned(), "draft.md".to_owned()][..])
        );
        assert!(!w.floating);
        assert_eq!(snap.focused_workspace.as_deref(), Some("3:code"));
    }

    #[test]
    fn focused_workspace_follows_a_focused_window_con() {
        // The real-world case: sway marks the focused *window con*
        // focused, and its ancestor Workspace node is NOT focused.
        // The old Workspace-only check returned None here (focus
        // restore silently dead); it must now resolve to "2:web".
        let win = build_node(
            4,
            Some("draft.md"),
            "con",
            true, // focused window
            vec![],
            vec![],
            json!({ "app_id": "neovide", "pid": 1234 }),
        );
        let ws = build_node(
            3,
            Some("2:web"),
            "workspace",
            false, // workspace node itself NOT focused
            vec![win],
            vec![],
            json!({}),
        );
        let output =
            build_node(2, Some("HDMI-0"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        let tree = from_value(root);
        let snap = capture_from_tree("x", &tree, |_| Some(vec!["neovide".into()]));
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.focused_workspace.as_deref(), Some("2:web"));
    }

    #[test]
    fn skips_scratchpad_windows() {
        let win = con(4, "private", "secret-app", Some(42));
        let ws = build_node(3, Some("__i3_scratch"), "workspace", false, vec![win], vec![], json!({}));
        let output = build_node(2, Some("__i3"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        let tree = from_value(root);
        let snap = capture_from_tree("x", &tree, |_| None);
        assert!(snap.windows.is_empty());
    }

    #[test]
    fn falls_back_to_window_class_for_xwayland() {
        let xwayland_slack = build_node(
            4,
            Some("Slack"),
            "con",
            false,
            vec![],
            vec![],
            json!({
                "pid": 55,
                "window_properties": {
                    "class": "Slack", "instance": "slack", "title": "Slack",
                    "window_role": null, "window_type": null, "transient_for": null
                }
            }),
        );
        let ws = build_node(3, Some("5:misc"), "workspace", false, vec![xwayland_slack], vec![], json!({}));
        let output = build_node(2, Some("out"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        let tree = from_value(root);
        let snap = capture_from_tree("x", &tree, |_| None);
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].app_id, "Slack");
    }

    #[test]
    fn descends_through_split_containers() {
        let tab1 = con(11, "tab1", "firefox", Some(101));
        let tab2 = con(12, "tab2", "firefox", Some(102));
        let split = build_node(10, None, "con", false, vec![tab1, tab2], vec![], json!({}));
        let ws = build_node(3, Some("1:web"), "workspace", false, vec![split], vec![], json!({}));
        let output = build_node(2, Some("out"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        let tree = from_value(root);
        let snap = capture_from_tree("x", &tree, |_| None);
        assert_eq!(snap.windows.len(), 2);
        assert!(snap.windows.iter().all(|w| w.app_id == "firefox"));
    }

    #[test]
    fn captures_floating_windows() {
        let mpv = build_node(
            4,
            Some("picture-in-picture"),
            "floating_con",
            false,
            vec![],
            vec![],
            json!({ "app_id": "mpv", "pid": 77 }),
        );
        let ws = build_node(3, Some("2:notes"), "workspace", false, vec![], vec![mpv], json!({}));
        let output = build_node(2, Some("out"), "output", false, vec![ws], vec![], json!({}));
        let root = build_node(1, Some("root"), "root", false, vec![output], vec![], json!({}));
        let tree = from_value(root);
        let snap = capture_from_tree("x", &tree, |_| None);
        assert_eq!(snap.windows.len(), 1);
        assert!(snap.windows[0].floating);
        assert_eq!(snap.windows[0].workspace, "2:notes");
    }

    #[test]
    fn parses_proc_cmdline_nul_separated() {
        let raw = b"neovide\0--fork\0draft.md\0";
        assert_eq!(
            parse_proc_cmdline(raw),
            Some(vec![
                "neovide".to_owned(),
                "--fork".to_owned(),
                "draft.md".to_owned()
            ])
        );
    }

    #[test]
    fn parses_empty_proc_cmdline_as_none() {
        assert_eq!(parse_proc_cmdline(&[]), None);
        assert_eq!(parse_proc_cmdline(&[0, 0, 0]), None);
    }

    #[test]
    fn missing_pid_yields_no_cmdline() {
        let tree = tree_one_window();
        let snap = capture_from_tree("x", &tree, |_pid| None);
        assert!(snap.windows[0].cmdline.is_none());
    }
}
