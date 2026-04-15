//! Workspace-switcher palette provider.
//!
//! Queries sway for the current workspace list on every `search()` call
//! (cheap — it's a single IPC round-trip) and offers each workspace as a
//! palette item. `execute()` runs `workspace <name>` via sway IPC.
//!
//! Opening a fresh `Connection` per search is intentional: it keeps the
//! provider stateless and independent of the lifetime of
//! [`crate::sway::SwayWorkspaceModule`]. On hosts without sway running
//! (CI, desktops on another compositor), every operation quietly returns
//! empty/no-op.

use async_trait::async_trait;
use swayipc_async::Connection;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const WORKSPACE_SWITCHER_PROVIDER: &str = "workspace-switcher";

pub struct WorkspaceSwitcherProvider;

impl WorkspaceSwitcherProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkspaceSwitcherProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PaletteProvider for WorkspaceSwitcherProvider {
    fn name(&self) -> &'static str {
        WORKSPACE_SWITCHER_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let mut conn = match Connection::new().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "workspace-switcher: sway IPC unavailable");
                return Vec::new();
            }
        };
        let workspaces = match conn.get_workspaces().await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "workspace-switcher: get_workspaces failed");
                return Vec::new();
            }
        };
        let q = query.to_ascii_lowercase();
        workspaces
            .into_iter()
            .filter_map(|ws| {
                let title = ws.name.clone();
                let name_lc = title.to_ascii_lowercase();
                let score = if q.is_empty() {
                    0.5
                } else if name_lc == q {
                    1.0
                } else if name_lc.starts_with(&q) {
                    0.85
                } else if name_lc.contains(&q) {
                    0.65
                } else {
                    return None;
                };
                let subtitle = if ws.focused {
                    format!("focused · {}", ws.output)
                } else {
                    ws.output.clone()
                };
                Some(
                    PaletteItem::new(WORKSPACE_SWITCHER_PROVIDER, ws.name.clone(), title)
                        .with_subtitle(subtitle)
                        .with_icon("workspace")
                        .with_score(score),
                )
            })
            .collect()
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        let mut conn = Connection::new().await.map_err(|e| {
            ProviderError::ExecuteFailed(format!("sway IPC unavailable: {e}"))
        })?;
        // Quote the workspace name with single quotes — sway's command
        // parser treats bare quotes specially. The item_id is the
        // workspace name, which can contain spaces.
        let cmd = format!("workspace '{}'", item_id.replace('\'', r"'\''"));
        let results = conn.run_command(&cmd).await.map_err(|e| {
            ProviderError::ExecuteFailed(format!("run_command failed: {e}"))
        })?;
        for r in results {
            if let Err(err) = r {
                tracing::warn!(error = %err, "workspace-switcher: sway command returned error");
            }
        }
        tracing::info!(workspace = %item_id, "workspace-switcher: switched");
        Ok(())
    }
}
