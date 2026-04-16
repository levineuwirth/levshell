//! CRUD operations for the `flashcards` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{Flashcard, FlashcardPatch, ListFlashcards, NewFlashcard};
use crate::store::DataStore;

const FLASHCARD_COLUMNS: &str =
    "id, front, back, linked_note_id, linked_ref_id, project_id, \
     interval_days, ease_factor, due_at, review_count, last_reviewed, \
     created_at, updated_at";

fn row_to_flashcard(row: &Row<'_>) -> rusqlite::Result<Flashcard> {
    Ok(Flashcard {
        id: row.get("id")?,
        front: row.get("front")?,
        back: row.get("back")?,
        linked_note_id: row.get("linked_note_id")?,
        linked_ref_id: row.get("linked_ref_id")?,
        project_id: row.get("project_id")?,
        interval_days: row.get("interval_days")?,
        ease_factor: row.get("ease_factor")?,
        due_at: row.get("due_at")?,
        review_count: row.get("review_count")?,
        last_reviewed: row.get("last_reviewed")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_flashcard(&self, new: NewFlashcard) -> Result<Flashcard> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO flashcards ({FLASHCARD_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, NULL, ?10, ?11) \
                 RETURNING {FLASHCARD_COLUMNS}"
            ))?;
            let card = stmt.query_row(
                params![
                    id,
                    new.front,
                    new.back,
                    new.linked_note_id,
                    new.linked_ref_id,
                    new.project_id,
                    new.interval_days,
                    new.ease_factor,
                    new.due_at,
                    now,
                    now,
                ],
                row_to_flashcard,
            )?;
            Ok(card)
        })
        .await
    }

    pub async fn get_flashcard(&self, id: Uuid) -> Result<Option<Flashcard>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {FLASHCARD_COLUMNS} FROM flashcards WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_flashcard) {
                Ok(f) => Ok(Some(f)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_flashcards(&self, params: ListFlashcards) -> Result<Vec<Flashcard>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {FLASHCARD_COLUMNS} FROM flashcards WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(project_id) = params.project_id {
                sql.push_str(" AND project_id = ?");
                binds.push(Value::Blob(project_id.as_bytes().to_vec()));
            }
            // See events.rs — match rusqlite's chrono storage format ("%F %T%.f%:z")
            // so string comparison on the TEXT column is correct.
            if let Some(due_before) = params.due_before {
                sql.push_str(" AND due_at <= ?");
                binds.push(Value::Text(due_before.format("%F %T%.f%:z").to_string()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'flashcard' AND tag = ?)",
                );
                binds.push(Value::Text(tag));
            }
            sql.push_str(" ORDER BY due_at ASC");
            if let Some(limit) = params.limit {
                sql.push_str(" LIMIT ?");
                binds.push(Value::Integer(limit as i64));
                if let Some(offset) = params.offset {
                    sql.push_str(" OFFSET ?");
                    binds.push(Value::Integer(offset as i64));
                }
            }

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(binds.iter()), row_to_flashcard)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_flashcard(&self, id: Uuid, patch: FlashcardPatch) -> Result<Flashcard> {
        self.with_conn(move |conn| {
            let now = Utc::now();

            let (set_note, note_val) = match patch.linked_note_id {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_ref, ref_val) = match patch.linked_ref_id {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_project, project_val) = match patch.project_id {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_reviewed, reviewed_val) = match patch.last_reviewed {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE flashcards SET \
                    front = COALESCE(?2, front), \
                    back = COALESCE(?3, back), \
                    linked_note_id = CASE WHEN ?4 THEN ?5 ELSE linked_note_id END, \
                    linked_ref_id = CASE WHEN ?6 THEN ?7 ELSE linked_ref_id END, \
                    project_id = CASE WHEN ?8 THEN ?9 ELSE project_id END, \
                    interval_days = COALESCE(?10, interval_days), \
                    ease_factor = COALESCE(?11, ease_factor), \
                    due_at = COALESCE(?12, due_at), \
                    review_count = COALESCE(?13, review_count), \
                    last_reviewed = CASE WHEN ?14 THEN ?15 ELSE last_reviewed END, \
                    updated_at = ?16 \
                 WHERE id = ?1 \
                 RETURNING {FLASHCARD_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![
                    id,
                    patch.front,
                    patch.back,
                    set_note, note_val,
                    set_ref, ref_val,
                    set_project, project_val,
                    patch.interval_days,
                    patch.ease_factor,
                    patch.due_at,
                    patch.review_count,
                    set_reviewed, reviewed_val,
                    now,
                ],
                row_to_flashcard,
            ) {
                Ok(f) => Ok(f),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_flashcard(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM flashcards WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
