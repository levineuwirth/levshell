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
pub mod module;
pub mod note_search;
pub mod provider;
pub mod workspace_switcher;

pub use app_launcher::AppLauncherProvider;
pub use module::{PaletteModule, PALETTE_WIDGET_ID, PALETTE_WIDGET_TYPE};
pub use note_search::NoteSearchProvider;
pub use provider::{merge_results, PaletteItem, PaletteProvider, PaletteState};
pub use workspace_switcher::{sway_switch_workspace, WorkspaceSwitcherProvider};
