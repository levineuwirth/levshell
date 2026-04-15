//! [`PaletteProvider`] trait + shared types.
//!
//! A provider is anything that can turn a free-text query into a list of
//! ranked [`PaletteItem`]s and then execute a selected item. The trait is
//! async so providers that hit a database (like the FTS5 note search) or
//! an external socket (like sway IPC) can do so without blocking.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One row in a palette result set. `id` is opaque to the palette module;
/// each provider picks an encoding that lets its `execute()` resolve the
/// item back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaletteItem {
    pub id: String,
    /// Matches [`PaletteProvider::name`] of the provider that produced it.
    /// The palette module uses this to route `execute()` calls back to
    /// the right provider.
    pub provider: String,
    /// Primary display text (e.g. "Firefox" or workspace name).
    pub title: String,
    /// Optional secondary text (e.g. "Web Browser", note snippet, exec path).
    pub subtitle: Option<String>,
    /// Optional icon *category hint* (provider-defined string the shell
    /// maps to a fallback glyph — `"app"`, `"workspace"`, `"note"`, …).
    /// Distinct from [`Self::icon_path`], which is a concrete filesystem
    /// path to an image file.
    pub icon: Option<String>,
    /// Optional absolute filesystem path to a rendered icon image
    /// (`.svg`, `.png`, `.xpm`). When present, the shell prefers this
    /// over the category-hint glyph. Populated today only by
    /// [`crate::palette::AppLauncherProvider`], which resolves
    /// `.desktop` `Icon=` values through the freedesktop icon theme
    /// search path at scan time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_path: Option<String>,
    /// Relevance score in [0.0, 1.0]. Higher = more relevant.
    pub score: f64,
}

impl PaletteItem {
    pub fn new(
        provider: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            title: title.into(),
            subtitle: None,
            icon: None,
            icon_path: None,
            score: 0.5,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn with_icon_path(mut self, icon_path: impl Into<String>) -> Self {
        self.icon_path = Some(icon_path.into());
        self
    }

    pub fn with_score(mut self, score: f64) -> Self {
        self.score = score.clamp(0.0, 1.0);
        self
    }
}

/// Serializable state payload for the palette widget. This is what the
/// daemon publishes in a `WidgetUpdate` and what the QML shell binds
/// against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub results: Vec<PaletteItem>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("item id not recognized: {0}")]
    UnknownItem(String),
    #[error("execute failed: {0}")]
    ExecuteFailed(String),
}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A palette provider produces results for a query and executes selected
/// items. Implementations should be cheap to construct and `Send + Sync`
/// so the palette module can hold a `Vec<Box<dyn PaletteProvider>>`.
#[async_trait]
pub trait PaletteProvider: Send + Sync {
    /// Short stable identifier (e.g. `"app-launcher"`, `"workspace-switcher"`,
    /// `"note-search"`). Used in the `provider` field of [`PaletteItem`]
    /// so `execute()` can be routed.
    fn name(&self) -> &'static str;

    /// Search for items matching `query`. An empty query is legal —
    /// providers may choose to return a default set (recent items, all
    /// workspaces, …) or an empty vec.
    async fn search(&self, query: &str) -> Vec<PaletteItem>;

    /// Execute the item identified by `item_id`. The palette module
    /// routes the call based on [`PaletteItem::provider`]. Errors are
    /// logged; Phase 1.5 doesn't propagate them to the shell.
    async fn execute(&self, item_id: &str) -> ProviderResult<()>;
}

/// Merge a set of per-provider result lists into a single ranked list,
/// capped at `limit` entries. Sort is by descending `score`; ties break
/// on `(provider, title)` for determinism.
pub fn merge_results(mut buckets: Vec<Vec<PaletteItem>>, limit: usize) -> Vec<PaletteItem> {
    let mut all: Vec<PaletteItem> = buckets.drain(..).flatten().collect();
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.title.cmp(&b.title))
    });
    all.truncate(limit);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sorts_by_descending_score() {
        let buckets = vec![
            vec![
                PaletteItem::new("a", "1", "Alpha").with_score(0.3),
                PaletteItem::new("a", "2", "Beta").with_score(0.9),
            ],
            vec![PaletteItem::new("b", "3", "Gamma").with_score(0.7)],
        ];
        let merged = merge_results(buckets, 10);
        assert_eq!(merged[0].title, "Beta");
        assert_eq!(merged[1].title, "Gamma");
        assert_eq!(merged[2].title, "Alpha");
    }

    #[test]
    fn merge_breaks_ties_stably_on_provider_then_title() {
        let buckets = vec![
            vec![
                PaletteItem::new("z-prov", "1", "Aaa").with_score(0.5),
                PaletteItem::new("z-prov", "2", "Bbb").with_score(0.5),
            ],
            vec![PaletteItem::new("a-prov", "3", "Ccc").with_score(0.5)],
        ];
        let merged = merge_results(buckets, 10);
        assert_eq!(merged[0].provider, "a-prov");
        assert_eq!(merged[1].title, "Aaa");
        assert_eq!(merged[2].title, "Bbb");
    }

    #[test]
    fn merge_truncates_to_limit() {
        let items: Vec<PaletteItem> = (0..20)
            .map(|i| PaletteItem::new("p", i.to_string(), format!("item {i}")).with_score(0.5))
            .collect();
        let merged = merge_results(vec![items], 5);
        assert_eq!(merged.len(), 5);
    }

    #[test]
    fn score_is_clamped_to_unit_range() {
        let over = PaletteItem::new("p", "1", "x").with_score(5.0);
        assert_eq!(over.score, 1.0);
        let under = PaletteItem::new("p", "2", "y").with_score(-0.5);
        assert_eq!(under.score, 0.0);
    }
}
