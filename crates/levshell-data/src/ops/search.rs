//! Full-text search over `notes` and `refs` via the FTS5 shadow tables.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::store::DataStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSearchHit {
    pub id: Uuid,
    pub title: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceSearchHit {
    pub id: Uuid,
    pub title: String,
    pub citekey: String,
    pub snippet: String,
}

impl DataStore {
    /// Run an FTS5 MATCH query against the notes index. The query string is
    /// passed verbatim to FTS5; callers wanting prefix search should append
    /// `*` themselves.
    pub async fn search_notes(&self, query: impl Into<String>, limit: u32) -> Result<Vec<NoteSearchHit>> {
        let query = query.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT n.id, n.title, snippet(notes_fts, 1, '<b>', '</b>', '...', 16) AS snip \
                 FROM notes n \
                 JOIN notes_fts ON notes_fts.rowid = n.rowid \
                 WHERE notes_fts MATCH ?1 \
                 ORDER BY rank \
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![query, limit as i64], |row| {
                    Ok(NoteSearchHit {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        snippet: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn search_references(
        &self,
        query: impl Into<String>,
        limit: u32,
    ) -> Result<Vec<ReferenceSearchHit>> {
        let query = query.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id, r.title, r.citekey, \
                        snippet(refs_fts, 1, '<b>', '</b>', '...', 16) AS snip \
                 FROM refs r \
                 JOIN refs_fts ON refs_fts.rowid = r.rowid \
                 WHERE refs_fts MATCH ?1 \
                 ORDER BY rank \
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![query, limit as i64], |row| {
                    Ok(ReferenceSearchHit {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        citekey: row.get(2)?,
                        snippet: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}
