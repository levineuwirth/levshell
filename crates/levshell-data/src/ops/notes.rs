//! CRUD operations for the `notes` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{ListNotes, NewNote, Note, NotePatch};
use crate::store::DataStore;

const NOTE_COLUMNS: &str = "id, title, content, project_id, created_at, updated_at";

fn row_to_note(row: &Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get("id")?,
        title: row.get("title")?,
        content: row.get("content")?,
        project_id: row.get("project_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_note(&self, new: NewNote) -> Result<Note> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();
            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO notes ({NOTE_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 RETURNING {NOTE_COLUMNS}"
            ))?;
            let note = stmt.query_row(
                params![id, new.title, new.content, new.project_id, now, now],
                row_to_note,
            )?;
            Ok(note)
        })
        .await
    }

    pub async fn get_note(&self, id: Uuid) -> Result<Option<Note>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {NOTE_COLUMNS} FROM notes WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_note) {
                Ok(n) => Ok(Some(n)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_notes(&self, params: ListNotes) -> Result<Vec<Note>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(project_id) = params.project_id {
                sql.push_str(" AND project_id = ?");
                binds.push(Value::Blob(project_id.as_bytes().to_vec()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'note' AND tag = ?)",
                );
                binds.push(Value::Text(tag));
            }
            sql.push_str(" ORDER BY updated_at DESC");
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
                .query_map(params_from_iter(binds.iter()), row_to_note)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_note(&self, id: Uuid, patch: NotePatch) -> Result<Note> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            // For project_id we cannot use COALESCE because the user may want
            // to clear the link (set to NULL). The patch wraps it in a double
            // Option: outer None = leave alone, outer Some(None) = unset.
            let (set_project, project_value): (bool, Option<Uuid>) = match patch.project_id {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE notes SET \
                    title = COALESCE(?2, title), \
                    content = COALESCE(?3, content), \
                    project_id = CASE WHEN ?4 THEN ?5 ELSE project_id END, \
                    updated_at = ?6 \
                 WHERE id = ?1 \
                 RETURNING {NOTE_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![id, patch.title, patch.content, set_project, project_value, now],
                row_to_note,
            ) {
                Ok(n) => Ok(n),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_note(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
