//! Command palette module and providers.
//!
//! The palette is an overlay UI launched by `levshell-ctl palette toggle`
//! (or a future keybind) that aggregates results from several **providers**:
//!
//! | Provider            | Source              | Execute action                 |
//! |---------------------|---------------------|--------------------------------|
//! | `app-launcher`      | `.desktop` files    | `Exec=` fork-exec              |
//! | `workspace-switcher`| sway `get_workspaces`| sway `workspace <name>`       |
//! | `note-search`       | SQLite FTS5         | (no-op in Phase 1.5; logs id)  |
//!
//! Each provider implements [`PaletteProvider`]; the [`PaletteModule`]
//! multiplexes them. When the user types in the palette, every provider's
//! `search()` is called concurrently and the results are merged by
//! descending `score` with a stable tiebreaker.

pub mod app_launcher;
pub mod calc;
pub mod module;
pub mod note_search;
pub mod provider;
pub mod recent_docs;
pub mod ref_search;
pub mod unicode;
pub mod workspace_switcher;

pub use app_launcher::AppLauncherProvider;
pub use calc::CalcProvider;
pub use module::{PaletteModule, PALETTE_WIDGET_ID, PALETTE_WIDGET_TYPE};
pub use note_search::NoteSearchProvider;
pub use provider::{merge_results, PaletteItem, PaletteProvider, PaletteState};
pub use recent_docs::RecentDocsProvider;
pub use ref_search::RefSearchProvider;
pub use unicode::UnicodeProvider;
pub use workspace_switcher::{sway_switch_workspace, WorkspaceSwitcherProvider};

use levshell_data::DataStore;

/// Spawn `program` with `args` fully detached from the daemon: no stdio,
/// its own process group (so daemon shutdown / Ctrl-C doesn't reap the
/// child). Shared by providers whose `execute()` opens a file
/// (`xdg-open`) or writes the clipboard (`wl-copy`).
pub(crate) fn spawn_detached(program: &str, args: &[&str]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ())
}

/// The canonical built-in palette provider set (spec §2.1.2 — the
/// palette as "the universal entry point"). Single registration point:
/// adding a provider is a one-line edit here and both the daemon and
/// integration tests get the same set (M3.14).
pub fn default_palette_providers(store: DataStore) -> Vec<Box<dyn PaletteProvider>> {
    vec![
        Box::new(AppLauncherProvider::new()),
        Box::new(WorkspaceSwitcherProvider::new()),
        Box::new(NoteSearchProvider::new(store.clone())),
        Box::new(RefSearchProvider::new(store)),
        Box::new(CalcProvider::new()),
        Box::new(UnicodeProvider::new()),
        Box::new(RecentDocsProvider::new()),
    ]
}
