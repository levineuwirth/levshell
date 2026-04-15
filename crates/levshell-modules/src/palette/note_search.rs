//! Note-search palette provider.
//!
//! Runs an FTS5 `MATCH` query against `notes_fts` via
//! [`DataStore::search_notes`]. The query is suffixed with `*` so users
//! get prefix matching as they type.
//!
//! Phase 1.5 `execute()` is a no-op beyond logging the note id — we don't
//! yet have an editor launcher or an in-shell note viewer. A later phase
//! will either publish a "focus note" bus event or launch $EDITOR.

use async_trait::async_trait;
use levshell_data::{DataStore, NoteSearchHit};

use super::provider::{PaletteItem, PaletteProvider, ProviderResult};

pub const NOTE_SEARCH_PROVIDER: &str = "note-search";

const MAX_HITS: u32 = 10;

pub struct NoteSearchProvider {
    store: DataStore,
}

impl NoteSearchProvider {
    pub fn new(store: DataStore) -> Self {
        Self { store }
    }
}

fn sanitize_query(query: &str) -> Option<String> {
    // FTS5 treats most punctuation as a token delimiter, but a few
    // characters (`"`, `*`, `(`, `)`, `-`) have query-language meaning.
    // For Phase 1.5 we strip them and append a trailing `*` so "fire"
    // still matches "firefly". Empty queries return None so the provider
    // skips the DB hit entirely.
    let cleaned: String = query
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .trim()
        .to_owned();
    if cleaned.is_empty() {
        return None;
    }
    // Split on whitespace and apply `*` to each term so every word
    // contributes a prefix match.
    let parts: Vec<String> = cleaned
        .split_whitespace()
        .map(|term| format!("{term}*"))
        .collect();
    Some(parts.join(" "))
}

fn hit_to_item(hit: NoteSearchHit, rank: usize) -> PaletteItem {
    // The FTS `rank` ordering is already relevance-sorted; we project
    // that into a decreasing score so the palette's cross-provider
    // merger ranks notes below near-perfect app launcher matches but
    // above substring hits.
    let score = 0.75 - (rank as f64 * 0.03).min(0.3);
    PaletteItem::new(NOTE_SEARCH_PROVIDER, hit.id.to_string(), hit.title)
        .with_subtitle(hit.snippet)
        .with_icon("note")
        .with_score(score)
}

#[async_trait]
impl PaletteProvider for NoteSearchProvider {
    fn name(&self) -> &'static str {
        NOTE_SEARCH_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let Some(fts_query) = sanitize_query(query) else {
            return Vec::new();
        };
        match self.store.search_notes(fts_query, MAX_HITS).await {
            Ok(hits) => hits
                .into_iter()
                .enumerate()
                .map(|(i, hit)| hit_to_item(hit, i))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "note-search: FTS query failed");
                Vec::new()
            }
        }
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        // Phase 1.5 placeholder: just log the selection. A later phase
        // will either publish a "focus note" bus event (picked up by a
        // note-viewer module) or spawn $EDITOR.
        tracing::info!(note_id = %item_id, "note-search: selected (no-op in Phase 1.5)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_punctuation() {
        assert_eq!(sanitize_query("firefox!"), Some("firefox*".to_string()));
        assert_eq!(
            sanitize_query("hello (world)"),
            Some("hello* world*".to_string())
        );
    }

    #[test]
    fn sanitize_empty_returns_none() {
        assert!(sanitize_query("").is_none());
        assert!(sanitize_query("   ").is_none());
        assert!(sanitize_query("!!!").is_none());
    }

    #[test]
    fn sanitize_adds_prefix_glob_per_term() {
        assert_eq!(
            sanitize_query("rust async trait"),
            Some("rust* async* trait*".to_string())
        );
    }
}
