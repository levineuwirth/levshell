//! CRUD operations for the `refs` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{ListReferences, NewReference, Reference, ReferencePatch};
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

    pub async fn list_references(&self, params: ListReferences) -> Result<Vec<Reference>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {REFERENCE_COLUMNS} FROM refs WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(project_id) = params.project_id {
                sql.push_str(" AND project_id = ?");
                binds.push(Value::Blob(project_id.as_bytes().to_vec()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'ref' AND tag = ?)",
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
                .query_map(params_from_iter(binds.iter()), row_to_reference)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_reference(&self, id: Uuid, patch: ReferencePatch) -> Result<Reference> {
        self.with_conn(move |conn| {
            let authors_json = patch.authors.map(|a| serde_json::to_string(&a)).transpose()?;
            let annotations_json = patch.annotations.map(|a| serde_json::to_string(&a)).transpose()?;
            let now = Utc::now();

            let (set_year, year_val) = match patch.year {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_venue, venue_val) = match patch.venue {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_doi, doi_val) = match patch.doi {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_abstract, abstract_val) = match patch.abstract_text {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_pdf, pdf_val) = match patch.pdf_path {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_progress, progress_val) = match patch.reading_progress {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_project, project_val) = match patch.project_id {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE refs SET \
                    title = COALESCE(?2, title), \
                    authors = COALESCE(?3, authors), \
                    year = CASE WHEN ?4 THEN ?5 ELSE year END, \
                    venue = CASE WHEN ?6 THEN ?7 ELSE venue END, \
                    doi = CASE WHEN ?8 THEN ?9 ELSE doi END, \
                    citekey = COALESCE(?10, citekey), \
                    abstract_text = CASE WHEN ?11 THEN ?12 ELSE abstract_text END, \
                    pdf_path = CASE WHEN ?13 THEN ?14 ELSE pdf_path END, \
                    reading_progress = CASE WHEN ?15 THEN ?16 ELSE reading_progress END, \
                    annotations = COALESCE(?17, annotations), \
                    project_id = CASE WHEN ?18 THEN ?19 ELSE project_id END, \
                    updated_at = ?20 \
                 WHERE id = ?1 \
                 RETURNING {REFERENCE_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![
                    id,
                    patch.title,
                    authors_json,
                    set_year, year_val,
                    set_venue, venue_val,
                    set_doi, doi_val,
                    patch.citekey,
                    set_abstract, abstract_val,
                    set_pdf, pdf_val,
                    set_progress, progress_val,
                    annotations_json,
                    set_project, project_val,
                    now,
                ],
                row_to_reference,
            ) {
                Ok(r) => Ok(r),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_reference(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM refs WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
