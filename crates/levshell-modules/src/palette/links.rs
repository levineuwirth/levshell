//! Relation-graph navigator palette provider (spec §2.1.2 + §5.1.1).
//!
//! The other search providers find an entity. This one finds an
//! entity's **connections**: type part of a note or reference title and
//! the results are the things it links to — the scaffolded literature
//! note for a paper, the papers a note cites, the notes a note
//! wiki-links. Selecting a result jumps you to that neighbour (opens a
//! reference's PDF, copies an `[[wikilink]]` / `@citekey` you can paste
//! into your editor).
//!
//! This is the only place the relation graph is *navigable* from
//! anywhere in the session — the connective tissue the unified model
//! exists for, one keystroke from the command palette.

use async_trait::async_trait;
use levshell_data::{DataStore, EntityType, RelationDirection};
use uuid::Uuid;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const LINKS_PROVIDER: &str = "links";
const MAX_HITS: u32 = 5;
const MAX_NEIGHBOURS: usize = 12;

pub struct LinksProvider {
    store: DataStore,
}

impl LinksProvider {
    pub fn new(store: DataStore) -> Self {
        Self { store }
    }
}

/// FTS sanitiser: keep alphanumerics/whitespace, append `*` per term
/// for prefix matching. `None` for an empty query (provider stays
/// silent rather than matching everything).
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

/// `"{type}|{uuid}"` — the palette item id, round-trippable so
/// `execute` knows which entity to act on. `type` is the DB form
/// (`note`, `ref`, …) so it matches [`EntityType::as_str`].
fn make_item_id(ty: EntityType, id: Uuid) -> String {
    format!("{}|{}", ty.as_str(), id)
}

fn parse_item_id(s: &str) -> Option<(EntityType, Uuid)> {
    let (ty, id) = s.split_once('|')?;
    Some((EntityType::from_db(ty).ok()?, Uuid::parse_str(id).ok()?))
}

/// Icon hint per neighbour type, reusing the palette's existing glyph
/// vocabulary (`note`/`ref` are already styled by NoteSearch/RefSearch).
fn neighbour_icon(ty: EntityType) -> &'static str {
    match ty {
        EntityType::Note => "note",
        EntityType::Reference => "ref",
        _ => "link",
    }
}

impl LinksProvider {
    /// The single best-matching anchor entity for `fts`: the top note
    /// hit if any (notes are the graph's hub via `wiki_link`), else the
    /// top reference. Returns its id/type/title.
    async fn best_anchor(&self, fts: &str) -> Option<(EntityType, Uuid, String)> {
        if let Ok(mut notes) = self.store.search_notes(fts, MAX_HITS).await {
            if !notes.is_empty() {
                let n = notes.remove(0);
                return Some((EntityType::Note, n.id, n.title));
            }
        }
        if let Ok(mut refs) = self.store.search_references(fts, MAX_HITS).await {
            if !refs.is_empty() {
                let r = refs.remove(0);
                return Some((EntityType::Reference, r.id, r.title));
            }
        }
        None
    }
}

#[async_trait]
impl PaletteProvider for LinksProvider {
    fn name(&self) -> &'static str {
        LINKS_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let Some(fts) = sanitize_query(query) else {
            return Vec::new();
        };
        let Some((anchor_ty, anchor_id, anchor_title)) = self.best_anchor(&fts).await
        else {
            return Vec::new();
        };
        let neighbours = match self.store.related_entities(anchor_id, anchor_ty).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "links: related_entities failed");
                return Vec::new();
            }
        };

        neighbours
            .into_iter()
            .take(MAX_NEIGHBOURS)
            .enumerate()
            .map(|(i, n)| {
                let arrow = match n.direction {
                    RelationDirection::Outgoing => "→",
                    RelationDirection::Incoming => "←",
                };
                // Below normal note/ref hits — this is a deliberate
                // "show me what connects" lookup, not a primary search.
                let score = 0.5 - (i as f64 * 0.02).min(0.2);
                PaletteItem::new(
                    LINKS_PROVIDER,
                    make_item_id(n.entity_type, n.entity_id),
                    n.label,
                )
                .with_subtitle(format!(
                    "{arrow} {} · from \u{201c}{anchor_title}\u{201d}",
                    n.kind
                ))
                .with_icon(neighbour_icon(n.entity_type))
                .with_score(score)
            })
            .collect()
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        let (ty, id) = parse_item_id(item_id)
            .ok_or_else(|| ProviderError::UnknownItem(item_id.to_owned()))?;

        // Resolve + act. Missing entity (raced deletion) is a no-op,
        // not an error — same posture as ref-search.
        match ty {
            EntityType::Reference => {
                let Some(r) = self
                    .store
                    .get_reference(id)
                    .await
                    .map_err(|e| ProviderError::ExecuteFailed(format!("get_reference: {e}")))?
                else {
                    return Ok(());
                };
                match r.pdf_path.as_deref().filter(|p| !p.is_empty()) {
                    Some(pdf) => super::spawn_detached("xdg-open", &[pdf])
                        .map_err(|e| ProviderError::ExecuteFailed(format!("open pdf: {e}")))?,
                    None => {
                        let cite = format!("@{}", r.citekey);
                        super::spawn_detached("wl-copy", &[&cite]).map_err(|e| {
                            ProviderError::ExecuteFailed(format!("wl-copy: {e}"))
                        })?;
                    }
                }
            }
            EntityType::Note => {
                let Some(n) = self
                    .store
                    .get_note(id)
                    .await
                    .map_err(|e| ProviderError::ExecuteFailed(format!("get_note: {e}")))?
                else {
                    return Ok(());
                };
                // An Obsidian-style wikilink the user can paste straight
                // into their editor to follow the connection.
                let link = format!("[[{}]]", n.title);
                super::spawn_detached("wl-copy", &[&link])
                    .map_err(|e| ProviderError::ExecuteFailed(format!("wl-copy: {e}")))?;
            }
            other => {
                // Only `scaffolded_from` (Note→Ref) and `wiki_link`
                // (Note→Note) edges exist today, so a neighbour is
                // always a Note or Reference. Other types can't be
                // reached here; if a future edge kind introduces one,
                // it's a no-op until this arm grows a real action,
                // rather than copying an opaque id.
                tracing::debug!(
                    entity_type = other.as_str(),
                    "links: no jump action for this neighbour type yet"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_data::{NewNote, NewReference};

    #[test]
    fn item_id_round_trips() {
        let id = Uuid::now_v7();
        let s = make_item_id(EntityType::Reference, id);
        assert_eq!(s, format!("ref|{id}"));
        assert_eq!(parse_item_id(&s), Some((EntityType::Reference, id)));

        let n = make_item_id(EntityType::Note, id);
        assert_eq!(parse_item_id(&n), Some((EntityType::Note, id)));
    }

    #[test]
    fn parse_item_id_rejects_garbage() {
        assert_eq!(parse_item_id("nope"), None);
        assert_eq!(parse_item_id("note|not-a-uuid"), None);
        assert_eq!(parse_item_id("bogus|"), None);
    }

    #[test]
    fn sanitize_appends_prefix_star() {
        assert_eq!(sanitize_query("  attention model "), Some("attention* model*".into()));
        assert_eq!(sanitize_query("   "), None);
    }

    #[tokio::test]
    async fn search_surfaces_the_anchor_neighbours() {
        let s = DataStore::open_in_memory().await.unwrap();
        let note = s
            .insert_note(NewNote {
                title: "Sparse Attention reading".into(),
                content: "notes about attention".into(),
                project_id: None,
            })
            .await
            .unwrap();
        let reference = s
            .insert_reference(NewReference {
                title: "Attention Is All You Need".into(),
                authors: vec!["Vaswani".into()],
                year: Some(2017),
                venue: None,
                doi: None,
                citekey: "vaswani2017".into(),
                abstract_text: None,
                pdf_path: None,
                reading_progress: None,
                annotations: vec![],
                project_id: None,
            })
            .await
            .unwrap();
        s.add_relation(
            note.id,
            EntityType::Note,
            reference.id,
            EntityType::Reference,
            "scaffolded_from",
        )
        .await
        .unwrap();

        let p = LinksProvider::new(s);
        let items = p.search("Sparse Attention").await;
        assert_eq!(items.len(), 1, "the note's one neighbour (the ref)");
        assert_eq!(items[0].title, "@vaswani2017 Attention Is All You Need");
        assert_eq!(items[0].provider, LINKS_PROVIDER);
        assert_eq!(
            parse_item_id(&items[0].id),
            Some((EntityType::Reference, reference.id))
        );
        let sub = items[0].subtitle.as_deref().unwrap();
        assert!(sub.contains("scaffolded_from"), "subtitle names the edge: {sub}");
        assert!(sub.contains('→'), "outgoing arrow from the note");
    }

    #[tokio::test]
    async fn empty_query_and_no_match_are_silent() {
        let s = DataStore::open_in_memory().await.unwrap();
        let p = LinksProvider::new(s);
        assert!(p.search("").await.is_empty());
        assert!(p.search("nothingmatchesthis").await.is_empty());
    }

    #[tokio::test]
    async fn execute_unknown_item_errors() {
        let s = DataStore::open_in_memory().await.unwrap();
        let p = LinksProvider::new(s);
        let err = p.execute("garbage").await.unwrap_err();
        assert!(matches!(err, ProviderError::UnknownItem(_)));
    }
}
