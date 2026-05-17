//! Note-search palette provider.
//!
//! Runs an FTS5 `MATCH` query against `notes_fts` via
//! [`DataStore::search_notes`]. The query is suffixed with `*` so users
//! get prefix matching as they type.
//!
//! `execute()` renders the stored note to a Markdown file in the
//! runtime dir and hands it to `xdg-open` (the user's default Markdown
//! handler / Obsidian). This is a read view — the synced source is
//! canonical; edits to the temp copy do not round-trip. A future
//! in-shell note viewer can supersede it.

use async_trait::async_trait;
use levshell_data::{DataStore, NoteSearchHit};

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

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
        // The PaletteItem id is the note's UUID (see `note_to_item`).
        let id = uuid::Uuid::parse_str(item_id).map_err(|e| {
            ProviderError::ExecuteFailed(format!("bad note id {item_id:?}: {e}"))
        })?;
        let note = self
            .store
            .get_note(id)
            .await
            .map_err(|e| ProviderError::ExecuteFailed(format!("get_note: {e}")))?;
        let Some(note) = note else {
            // Note vanished between search and select (e.g. an Obsidian
            // delete synced in). Not fatal — nothing to open.
            tracing::info!(note_id = %item_id, "note-search: note no longer exists");
            return Ok(());
        };

        // Self-contained "open": render the stored note to a Markdown
        // file in the runtime dir and hand it to xdg-open (the user's
        // default Markdown handler / Obsidian). This is a read view —
        // the canonical copy is the synced source; edits here do not
        // round-trip back. A future note-viewer module can supersede
        // this, but it makes selection a real action today.
        let dir = std::env::var("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("levshell");
        std::fs::create_dir_all(&dir).map_err(|e| {
            ProviderError::ExecuteFailed(format!("create {}: {e}", dir.display()))
        })?;
        let path = dir.join(format!("note-{id}.md"));
        let body = format!("# {}\n\n{}\n", note.title, note.content);
        std::fs::write(&path, body).map_err(|e| {
            ProviderError::ExecuteFailed(format!("write {}: {e}", path.display()))
        })?;

        use std::process::{Command, Stdio};
        let mut cmd = Command::new("xdg-open");
        cmd.arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.spawn().map_err(|e| {
            ProviderError::ExecuteFailed(format!("spawn xdg-open: {e}"))
        })?;
        tracing::info!(note_id = %item_id, path = %path.display(), "note-search: opened");
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
