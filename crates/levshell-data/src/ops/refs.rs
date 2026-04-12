//! Minimal `refs` operations: insert and get, used to back FTS search and to
//! seed the integration tests. Full CRUD for references is deferred to a
//! later step (Phase 1+).

use chrono::Utc;
use rusqlite::{params, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::models::{NewReference, Reference};
use crate::store::DataStore;

const REFERENCE_COLUMNS: &str =
    "id, title, authors, year, venue, doi, citekey, abstract_text, pdf_path, \
     reading_progress, annotations, project_id, created_at, updated_at";

pub(crate) fn row_to_reference(row: &Row<'_>) -> rusqlite::Result<Reference> {
    let authors_json: String = row.get("authors")?;
    let authors: Vec<String> = serde_json::from_str(&authors_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let annotations_json: String = row.get("annotations")?;
    let annotations: Vec<String> = serde_json::from_str(&annotations_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Reference {
        id: row.get("id")?,
        title: row.get("title")?,
        authors,
        year: row.get("year")?,
        venue: row.get("venue")?,
        doi: row.get("doi")?,
        citekey: row.get("citekey")?,
        abstract_text: row.get("abstract_text")?,
        pdf_path: row.get("pdf_path")?,
        reading_progress: row.get("reading_progress")?,
        annotations,
        project_id: row.get("project_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_reference(&self, new: NewReference) -> Result<Reference> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();
            let authors_json = serde_json::to_string(&new.authors)?;
            let annotations_json = serde_json::to_string(&new.annotations)?;

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO refs ({REFERENCE_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 RETURNING {REFERENCE_COLUMNS}"
            ))?;
            let reference = stmt.query_row(
                params![
                    id,
                    new.title,
                    authors_json,
                    new.year,
                    new.venue,
                    new.doi,
                    new.citekey,
                    new.abstract_text,
                    new.pdf_path,
                    new.reading_progress,
                    annotations_json,
                    new.project_id,
                    now,
                    now,
                ],
                row_to_reference,
            )?;
            Ok(reference)
        })
        .await
    }

    pub async fn get_reference(&self, id: Uuid) -> Result<Option<Reference>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {REFERENCE_COLUMNS} FROM refs WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_reference) {
                Ok(r) => Ok(Some(r)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }
}
