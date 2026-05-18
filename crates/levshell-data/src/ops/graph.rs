//! Relation-graph read primitive (spec §5.1.1 — the unified model is
//! "more than a bag of isolated tools" only if the edges are
//! *readable*, not just writable).
//!
//! [`add_relation`]/[`list_relations_from`]/[`list_relations_to`] give
//! raw `(id, type)` pairs. Every consumer that wants to *show* the graph
//! (a backlink count, a "jump to connected entity" palette, a
//! graph-mined nudge) then has to re-implement the same two-direction
//! union + per-type label resolution. This module does it once.
//!
//! [`DataStore::related_entities`] returns one [`RelatedEntity`] per
//! edge touching the given entity, in *either* direction, with the
//! neighbour resolved to a human label. Dangling edges (a relation
//! whose other endpoint was deleted) are dropped rather than surfaced —
//! a deleted note must not leave a ghost row in someone's backlink
//! list.
//!
//! [`add_relation`]: crate::store::DataStore::add_relation
//! [`list_relations_from`]: crate::store::DataStore::list_relations_from
//! [`list_relations_to`]: crate::store::DataStore::list_relations_to

use uuid::Uuid;

use crate::error::Result;
use crate::models::EntityType;
use crate::store::DataStore;
use serde::{Deserialize, Serialize};

/// Which way the edge points relative to the queried entity. An
/// `Outgoing` edge is one the queried entity declared (it is the
/// `source`); `Incoming` is a backlink (the queried entity is the
/// `target`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationDirection {
    Outgoing,
    Incoming,
}

/// One resolved neighbour in the relation graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedEntity {
    /// `entity_relations.relation_kind` (`scaffolded_from`, `wiki_link`,
    /// …). Kept verbatim so callers can group/filter by edge type.
    pub kind: String,
    pub direction: RelationDirection,
    pub entity_type: EntityType,
    pub entity_id: Uuid,
    /// Human label for the neighbour: note/event/task title, project /
    /// experiment name, `@citekey title` for a reference, a truncated
    /// front for a flashcard.
    pub label: String,
}

impl DataStore {
    /// Resolve a single entity to a display label, or `None` if it no
    /// longer exists (dangling-edge guard).
    async fn entity_label(&self, ty: EntityType, id: Uuid) -> Result<Option<String>> {
        Ok(match ty {
            EntityType::Note => self.get_note(id).await?.map(|n| n.title),
            EntityType::Project => self.get_project(id).await?.map(|p| p.name),
            EntityType::Reference => self.get_reference(id).await?.map(|r| {
                let t = r.title.trim();
                if t.is_empty() {
                    format!("@{}", r.citekey)
                } else {
                    format!("@{} {t}", r.citekey)
                }
            }),
            EntityType::Flashcard => self.get_flashcard(id).await?.map(|f| {
                let front = f.front.trim();
                if front.chars().count() > 60 {
                    let cut: String = front.chars().take(60).collect();
                    format!("{cut}…")
                } else {
                    front.to_owned()
                }
            }),
            EntityType::Event => self.get_event(id).await?.map(|e| e.title),
            EntityType::Task => self.get_task(id).await?.map(|t| t.title),
            EntityType::Experiment => self.get_experiment(id).await?.map(|x| x.name),
        })
    }

    /// All graph neighbours of `(id, ty)` in both directions, neighbour
    /// labels resolved, dangling edges dropped.
    ///
    /// Sorted by `(kind, label)` so a rendered list is stable across
    /// calls and a count is order-independent.
    pub async fn related_entities(
        &self,
        id: Uuid,
        ty: EntityType,
    ) -> Result<Vec<RelatedEntity>> {
        let mut out: Vec<RelatedEntity> = Vec::new();

        for rel in self.list_relations_from(id, ty).await? {
            if let Some(label) = self
                .entity_label(rel.target_type, rel.target_id)
                .await?
            {
                out.push(RelatedEntity {
                    kind: rel.kind,
                    direction: RelationDirection::Outgoing,
                    entity_type: rel.target_type,
                    entity_id: rel.target_id,
                    label,
                });
            }
        }
        for rel in self.list_relations_to(id, ty).await? {
            if let Some(label) = self
                .entity_label(rel.source_type, rel.source_id)
                .await?
            {
                out.push(RelatedEntity {
                    kind: rel.kind,
                    direction: RelationDirection::Incoming,
                    entity_type: rel.source_type,
                    entity_id: rel.source_id,
                    label,
                });
            }
        }

        out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.label.cmp(&b.label)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NewNote, NewReference};

    async fn store() -> DataStore {
        DataStore::open_in_memory().await.unwrap()
    }

    fn newref(citekey: &str, title: &str) -> NewReference {
        NewReference {
            title: title.into(),
            authors: vec!["A".into()],
            year: Some(2024),
            venue: None,
            doi: None,
            citekey: citekey.into(),
            abstract_text: None,
            pdf_path: None,
            reading_progress: None,
            annotations: vec![],
            project_id: None,
        }
    }

    #[tokio::test]
    async fn resolves_both_directions_with_labels() {
        let s = store().await;
        let note = s
            .insert_note(NewNote {
                title: "Literature notes — Attention".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap();
        let reference = s
            .insert_reference(newref("vaswani2017", "Attention Is All You Need"))
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

        // From the note's side: one outgoing edge to the reference.
        let from_note = s.related_entities(note.id, EntityType::Note).await.unwrap();
        assert_eq!(from_note.len(), 1);
        assert_eq!(from_note[0].direction, RelationDirection::Outgoing);
        assert_eq!(from_note[0].entity_type, EntityType::Reference);
        assert_eq!(from_note[0].entity_id, reference.id);
        assert_eq!(from_note[0].kind, "scaffolded_from");
        assert_eq!(from_note[0].label, "@vaswani2017 Attention Is All You Need");

        // From the reference's side: the same edge, seen as a backlink.
        let to_ref = s
            .related_entities(reference.id, EntityType::Reference)
            .await
            .unwrap();
        assert_eq!(to_ref.len(), 1);
        assert_eq!(to_ref[0].direction, RelationDirection::Incoming);
        assert_eq!(to_ref[0].entity_type, EntityType::Note);
        assert_eq!(to_ref[0].label, "Literature notes — Attention");
    }

    #[tokio::test]
    async fn dangling_edge_is_dropped() {
        let s = store().await;
        let note = s
            .insert_note(NewNote {
                title: "Keeper".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap();
        // Edge to a reference id that was never inserted.
        let ghost = Uuid::now_v7();
        s.add_relation(
            note.id,
            EntityType::Note,
            ghost,
            EntityType::Reference,
            "wiki_link",
        )
        .await
        .unwrap();

        let neighbours = s.related_entities(note.id, EntityType::Note).await.unwrap();
        assert!(
            neighbours.is_empty(),
            "an edge to a deleted/absent entity must not surface"
        );
    }

    #[tokio::test]
    async fn sorted_by_kind_then_label() {
        let s = store().await;
        let hub = s
            .insert_note(NewNote {
                title: "Hub".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap();
        let zeta = s
            .insert_note(NewNote {
                title: "Zeta".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap()
            .id;
        let alpha = s
            .insert_note(NewNote {
                title: "Alpha".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap()
            .id;
        for tgt in [zeta, alpha] {
            s.add_relation(hub.id, EntityType::Note, tgt, EntityType::Note, "wiki_link")
                .await
                .unwrap();
        }
        let n = s.related_entities(hub.id, EntityType::Note).await.unwrap();
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].label, "Alpha");
        assert_eq!(n[1].label, "Zeta");
    }

    #[tokio::test]
    async fn no_edges_is_empty() {
        let s = store().await;
        let note = s
            .insert_note(NewNote {
                title: "Lonely".into(),
                content: "x".into(),
                project_id: None,
            })
            .await
            .unwrap();
        assert!(s
            .related_entities(note.id, EntityType::Note)
            .await
            .unwrap()
            .is_empty());
    }
}
