//! Annotation → literature-note scaffolding (spec §2.9.8 / §3.3.3).
//!
//! "A Reference's annotations can be automatically scaffolded into
//! Notes" — this is the model-level transform that makes the unified
//! data model more than a bag of isolated tools. It is **source-
//! agnostic**: the Zotero adapter calls it today, but a native
//! reference manager (or Obsidian import) gets the identical behaviour
//! for free, because it operates on the `Reference` entity, not on any
//! tool's format (spec §5.1.1).
//!
//! Idempotent: the scaffolded note is linked back to its reference with
//! a [`SCAFFOLD_RELATION_KIND`] edge (`note --scaffolded_from--> ref`).
//! Re-running updates that note in place rather than spawning duplicates,
//! so it is safe to call on every sync pass.

use uuid::Uuid;

use crate::error::Result;
use crate::models::{EntityType, NewNote, NotePatch, Reference};
use crate::store::DataStore;

/// `entity_relations.relation_kind` tagging a note the system generated
/// from a reference's annotations. The edge is `Note -> Reference`.
pub const SCAFFOLD_RELATION_KIND: &str = "scaffolded_from";

fn render(reference: &Reference) -> (String, String) {
    let title = format!("Literature notes — {}", reference.title);
    let mut body = format!("# {title}\n\n");
    let mut cite = format!("@{}", reference.citekey);
    if let Some(y) = reference.year {
        cite.push_str(&format!(" · {y}"));
    }
    body.push_str(&format!("> {cite}\n\n## Highlights\n\n"));
    for ann in &reference.annotations {
        let line = ann.trim();
        if !line.is_empty() {
            body.push_str("- ");
            body.push_str(line);
            body.push('\n');
        }
    }
    body.push_str(
        "\n---\n*Scaffolded from imported annotations. Edit freely — \
         this note is native and will not be overwritten once you change \
         its title.*\n",
    );
    (title, body)
}

impl DataStore {
    /// Create or refresh the literature note scaffolded from
    /// `reference`'s annotations.
    ///
    /// * Empty `annotations` → `Ok(None)` (nothing to scaffold; an
    ///   existing scaffold is intentionally left untouched rather than
    ///   deleted — the user may have kept editing it).
    /// * No existing scaffold → insert a note + a
    ///   [`SCAFFOLD_RELATION_KIND`] edge, return its id.
    /// * Existing scaffold → refresh its content in place, return its id.
    ///
    /// Returns the note id when one was written.
    pub async fn scaffold_note_for_reference(
        &self,
        reference: &Reference,
    ) -> Result<Option<Uuid>> {
        if reference.annotations.iter().all(|a| a.trim().is_empty()) {
            return Ok(None);
        }
        let (title, content) = render(reference);

        // Existing scaffold? Look for an incoming Note edge of our kind.
        let existing = self
            .list_relations_to(reference.id, EntityType::Reference)
            .await?
            .into_iter()
            .find(|r| {
                r.kind == SCAFFOLD_RELATION_KIND && r.source_type == EntityType::Note
            })
            .map(|r| r.source_id);

        if let Some(note_id) = existing {
            // Refresh content, but if the user renamed the note (its
            // title no longer carries the generated prefix) leave the
            // title alone — their edit wins.
            let current = self.get_note(note_id).await?;
            let user_renamed = current
                .as_ref()
                .map(|n| !n.title.starts_with("Literature notes — "))
                .unwrap_or(false);
            self.update_note(
                note_id,
                NotePatch {
                    title: if user_renamed { None } else { Some(title) },
                    content: Some(content),
                    ..Default::default()
                },
            )
            .await?;
            Ok(Some(note_id))
        } else {
            let note = self
                .insert_note(NewNote {
                    title,
                    content,
                    project_id: reference.project_id,
                })
                .await?;
            self.add_relation(
                note.id,
                EntityType::Note,
                reference.id,
                EntityType::Reference,
                SCAFFOLD_RELATION_KIND,
            )
            .await?;
            Ok(Some(note.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::NewReference;
    use chrono::Utc;

    fn reference_with(annotations: Vec<String>) -> NewReference {
        NewReference {
            title: "Attention Is All You Need".into(),
            authors: vec!["Vaswani".into()],
            year: Some(2017),
            venue: None,
            doi: None,
            citekey: "vaswani2017".into(),
            abstract_text: None,
            pdf_path: None,
            reading_progress: None,
            annotations,
            project_id: None,
        }
    }

    async fn store() -> DataStore {
        DataStore::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn empty_annotations_scaffold_nothing() {
        let s = store().await;
        let r = s.insert_reference(reference_with(vec![])).await.unwrap();
        assert_eq!(s.scaffold_note_for_reference(&r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn scaffold_creates_linked_note_once_then_updates() {
        let s = store().await;
        let r = s
            .insert_reference(reference_with(vec![
                "self-attention scales".into(),
                "no recurrence".into(),
            ]))
            .await
            .unwrap();

        let id1 = s.scaffold_note_for_reference(&r).await.unwrap().unwrap();
        let note = s.get_note(id1).await.unwrap().unwrap();
        assert!(note.title.starts_with("Literature notes —"));
        assert!(note.content.contains("- self-attention scales"));
        assert!(note.content.contains("@vaswani2017"));

        // The Note->Reference scaffold edge exists.
        let edges = s
            .list_relations_to(r.id, EntityType::Reference)
            .await
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, SCAFFOLD_RELATION_KIND);
        assert_eq!(edges[0].source_id, id1);

        // Re-run with more annotations → same note id, refreshed body,
        // no duplicate edge.
        let mut r2 = r.clone();
        r2.annotations.push("residual + layernorm".into());
        r2.updated_at = Utc::now();
        let id2 = s.scaffold_note_for_reference(&r2).await.unwrap().unwrap();
        assert_eq!(id1, id2, "must reuse the existing scaffold note");
        let note2 = s.get_note(id2).await.unwrap().unwrap();
        assert!(note2.content.contains("residual + layernorm"));
        assert_eq!(
            s.list_relations_to(r.id, EntityType::Reference)
                .await
                .unwrap()
                .len(),
            1,
            "no duplicate scaffold edge"
        );
    }

    #[tokio::test]
    async fn user_renamed_scaffold_keeps_its_title() {
        let s = store().await;
        let r = s
            .insert_reference(reference_with(vec!["a point".into()]))
            .await
            .unwrap();
        let id = s.scaffold_note_for_reference(&r).await.unwrap().unwrap();
        s.update_note(
            id,
            NotePatch {
                title: Some("My transformer notes".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Re-scaffold: content refreshes, the user's title survives.
        let r2 = r.clone();
        s.scaffold_note_for_reference(&r2).await.unwrap();
        let note = s.get_note(id).await.unwrap().unwrap();
        assert_eq!(note.title, "My transformer notes");
    }
}
