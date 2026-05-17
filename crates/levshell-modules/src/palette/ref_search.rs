//! Reference-search palette provider (spec §2.1.2 / §2.9.8).
//!
//! FTS5 `MATCH` against `refs_fts` via [`DataStore::search_references`].
//! Selecting a hit opens its PDF if one is on disk, otherwise copies the
//! formatted `@citekey` to the Wayland clipboard ("citation quick-search
//! … copy formatted citekeys", spec §2.9.8).

use async_trait::async_trait;
use levshell_data::{DataStore, ReferenceSearchHit};

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const REF_SEARCH_PROVIDER: &str = "ref-search";
const MAX_HITS: u32 = 10;

pub struct RefSearchProvider {
    store: DataStore,
}

impl RefSearchProvider {
    pub fn new(store: DataStore) -> Self {
        Self { store }
    }
}

/// Same FTS sanitiser as note-search: strip query-language punctuation,
/// append `*` per term for prefix matching, `None` for an empty query.
fn sanitize_query(query: &str) -> Option<String> {
    let cleaned: String = query
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .trim()
        .to_owned();
    if cleaned.is_empty() {
        return None;
    }
    Some(
        cleaned
            .split_whitespace()
            .map(|t| format!("{t}*"))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn hit_to_item(hit: ReferenceSearchHit, rank: usize) -> PaletteItem {
    // Sit just below note hits in the cross-provider merge — references
    // are usually a deliberate lookup, but app/exact matches still win.
    let score = 0.72 - (rank as f64 * 0.03).min(0.3);
    PaletteItem::new(REF_SEARCH_PROVIDER, hit.id.to_string(), hit.title)
        .with_subtitle(format!("@{}  {}", hit.citekey, hit.snippet))
        .with_icon("ref")
        .with_score(score)
}

#[async_trait]
impl PaletteProvider for RefSearchProvider {
    fn name(&self) -> &'static str {
        REF_SEARCH_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let Some(fts) = sanitize_query(query) else {
            return Vec::new();
        };
        match self.store.search_references(fts, MAX_HITS).await {
            Ok(hits) => hits
                .into_iter()
                .enumerate()
                .map(|(i, h)| hit_to_item(h, i))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "ref-search: FTS query failed");
                Vec::new()
            }
        }
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        let id = uuid::Uuid::parse_str(item_id).map_err(|e| {
            ProviderError::ExecuteFailed(format!("bad ref id {item_id:?}: {e}"))
        })?;
        let reference = self
            .store
            .get_reference(id)
            .await
            .map_err(|e| ProviderError::ExecuteFailed(format!("get_reference: {e}")))?;
        let Some(reference) = reference else {
            tracing::info!(ref_id = %item_id, "ref-search: reference no longer exists");
            return Ok(());
        };

        match reference.pdf_path.as_deref().filter(|p| !p.is_empty()) {
            Some(pdf) => {
                super::spawn_detached("xdg-open", &[pdf]).map_err(|e| {
                    ProviderError::ExecuteFailed(format!("open pdf: {e}"))
                })?;
                tracing::info!(ref_id = %item_id, pdf, "ref-search: opened PDF");
            }
            None => {
                let citekey = format!("@{}", reference.citekey);
                super::spawn_detached("wl-copy", &[&citekey]).map_err(|e| {
                    ProviderError::ExecuteFailed(format!("wl-copy: {e}"))
                })?;
                tracing::info!(ref_id = %item_id, citekey, "ref-search: copied citekey");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_prefix_globs_each_term() {
        assert_eq!(
            sanitize_query("attention is all"),
            Some("attention* is* all*".to_string())
        );
        assert!(sanitize_query("  ").is_none());
    }

    #[test]
    fn hit_subtitle_carries_citekey() {
        let item = hit_to_item(
            ReferenceSearchHit {
                id: uuid::Uuid::nil(),
                title: "Attention Is All You Need".into(),
                citekey: "vaswani2017".into(),
                snippet: "…transformer…".into(),
            },
            0,
        );
        assert!(item.subtitle.unwrap().starts_with("@vaswani2017"));
        assert_eq!(item.provider, REF_SEARCH_PROVIDER);
    }
}
