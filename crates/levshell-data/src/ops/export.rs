//! Whole-store durability: a portable, inspectable snapshot of every
//! row in the unified database, and a faithful restore of one.
//!
//! ## Why this is generic, not per-entity
//!
//! Every other op module hand-maps one table's columns to a typed
//! struct. A typed export would have to re-implement all of that
//! (JSON-array columns, enum→string, computed columns) a second time —
//! a parallel mapping that silently drifts from the schema the moment a
//! column is added. Instead this captures each row *as the database
//! holds it*: column name → JSON scalar, with `BLOB` columns (the
//! UUID-v7 ids) hex-encoded. That is faithful by construction — it
//! round-trips the exact stored representation, including ids and the
//! space-separated timestamp format — and adding a column to any table
//! needs zero changes here.
//!
//! ## Trust properties
//!
//! - **Inspectable.** The artifact is JSON keyed by real table and
//!   column names; a researcher can read, grep, or migrate their data
//!   without this program. The unified store stops being a single
//!   opaque SQLite file you have to trust.
//! - **Versioned.** [`EXPORT_VERSION`] is stamped in; [`DataStore::import_all`]
//!   refuses a snapshot it doesn't understand rather than corrupting a
//!   store.
//! - **Restore-only.** Import refuses a non-empty store: it reconstructs
//!   a backup into a fresh database, it never silently merges into or
//!   half-overwrites a live one.
//! - **Atomic.** The whole import is one transaction — it fully applies
//!   or leaves the target untouched.
//!
//! The two FTS5 shadow tables are deliberately excluded: they are
//! trigger-maintained, so re-inserting the base `notes`/`refs` rows
//! repopulates them automatically.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{DataError, Result};
use crate::store::DataStore;

/// Bumped whenever the snapshot shape or the set of exported tables
/// changes incompatibly. `import_all` hard-rejects a mismatch.
pub const EXPORT_VERSION: u32 = 1;

/// Every persistent table, in foreign-key-safe insert order (parents
/// before children). The `*_fts` virtual tables are intentionally
/// absent — FTS is rebuilt by `AFTER INSERT` triggers on `notes`/`refs`.
const TABLES: &[&str] = &[
    "projects",
    "notes",
    "refs",
    "flashcards",
    "events",
    "tasks",
    "experiments",
    "entity_tags",
    "entity_relations",
    "sync_metadata",
];

/// One row as the DB holds it: column name → JSON scalar. `BLOB`
/// columns are lowercase-hex strings; everything else is the natural
/// JSON scalar (TEXT→string, INTEGER→number, REAL→number, NULL→null).
pub type RowMap = serde_json::Map<String, serde_json::Value>;

/// A complete, portable snapshot of the data store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreExport {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    /// Table name → its rows, in primary-key/scan order.
    pub tables: BTreeMap<String, Vec<RowMap>>,
}

impl StoreExport {
    /// Total rows across every table — the headline number for a
    /// human-readable "exported N records" summary.
    pub fn row_count(&self) -> usize {
        self.tables.values().map(Vec::len).sum()
    }
}

/// Per-table count of rows written by [`DataStore::import_all`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub inserted: BTreeMap<String, usize>,
}

impl ImportReport {
    pub fn total(&self) -> usize {
        self.inserted.values().sum()
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(DataError::Export(format!("odd-length hex blob: {s:?}")));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| DataError::Export(format!("malformed hex blob {s:?}: {e}")))
        })
        .collect()
}

fn vref_to_json(v: ValueRef<'_>) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        ValueRef::Null => J::Null,
        ValueRef::Integer(i) => J::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(J::Number)
            .unwrap_or(J::Null),
        ValueRef::Text(t) => J::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => J::String(to_hex(b)),
    }
}

fn json_to_sql(v: &serde_json::Value, is_blob: bool) -> Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Integer(i64::from(*b)),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(u) = n.as_u64() {
                Value::Integer(u as i64)
            } else {
                Value::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => {
            if is_blob {
                Value::Blob(from_hex(s)?)
            } else {
                Value::Text(s.clone())
            }
        }
        // JSON-array/object columns (open_questions, authors, …) are
        // stored as TEXT in SQLite, so they came back as a String
        // above; reaching here means an unexpected shape — keep it as
        // text rather than dropping data.
        other => Value::Text(other.to_string()),
    })
}

/// Names of the `BLOB`-typed columns of `table`, so import can hex-decode
/// exactly those back to bytes.
fn blob_columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    // `table` is always a literal from `TABLES`, never caller input.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| {
        let name: String = r.get(1)?;
        let ty: String = r.get(2)?;
        Ok((name, ty))
    })?;
    let mut set = HashSet::new();
    for r in rows {
        let (name, ty) = r?;
        if ty.to_ascii_uppercase().starts_with("BLOB") {
            set.insert(name);
        }
    }
    Ok(set)
}

/// User tables physically present in the schema, excluding SQLite
/// internals (`sqlite_%`) and the FTS5 shadow tables (`%_fts%`, rebuilt
/// by triggers and never exported).
fn live_user_tables(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
           AND name NOT LIKE '%\\_fts%' ESCAPE '\\'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Fail loudly if the curated [`TABLES`] list no longer covers the live
/// schema. `TABLES` is hand-ordered for FK-safe insert, so it can't be
/// `sqlite_master`-driven — but a future migration that adds a table
/// must not silently produce an incomplete backup (or, on import, leave
/// the new table's emptiness unchecked). This converts that latent data
/// loss into an immediate, actionable error: bump [`EXPORT_VERSION`]
/// and add the table to `TABLES` in FK order.
fn assert_table_list_covers_schema(conn: &Connection) -> Result<()> {
    let known: HashSet<&str> = TABLES.iter().copied().collect();
    let mut missing: Vec<String> = live_user_tables(conn)?
        .into_iter()
        .filter(|t| !known.contains(t.as_str()))
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(DataError::Export(format!(
            "snapshot table list is stale: schema has table(s) {missing:?} not \
             in TABLES — refusing to {} an incomplete backup (add them to \
             TABLES in FK-safe order and bump EXPORT_VERSION)",
            "produce/restore against"
        )));
    }
    Ok(())
}

impl DataStore {
    /// Capture every persistent row into a portable [`StoreExport`].
    /// Read-only; safe to call on a live store.
    pub async fn export_all(&self) -> Result<StoreExport> {
        self.with_conn(move |conn| {
            // A new migration must fail here, not silently omit a table.
            assert_table_list_covers_schema(conn)?;
            let mut tables: BTreeMap<String, Vec<RowMap>> = BTreeMap::new();
            for &table in TABLES {
                let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
                let cols: Vec<String> =
                    stmt.column_names().into_iter().map(str::to_string).collect();
                let mut q = stmt.query([])?;
                let mut rows = Vec::new();
                while let Some(row) = q.next()? {
                    let mut obj = RowMap::new();
                    for (i, c) in cols.iter().enumerate() {
                        obj.insert(c.clone(), vref_to_json(row.get_ref(i)?));
                    }
                    rows.push(obj);
                }
                tables.insert(table.to_string(), rows);
            }
            Ok(StoreExport {
                version: EXPORT_VERSION,
                exported_at: Utc::now(),
                tables,
            })
        })
        .await
    }

    /// Reconstruct a [`StoreExport`] into this store. Rejects a version
    /// it doesn't understand and any non-empty target (restore is not
    /// a merge). All-or-nothing: one transaction.
    pub async fn import_all(&self, export: StoreExport) -> Result<ImportReport> {
        self.with_conn(move |conn| {
            if export.version != EXPORT_VERSION {
                return Err(DataError::Export(format!(
                    "snapshot version {} != supported {EXPORT_VERSION}",
                    export.version
                )));
            }

            // Don't silently drop a table the snapshot carries but this
            // binary doesn't know — that is exactly the data loss this
            // feature exists to prevent. (The version gate above should
            // already catch this; this is defence in depth and a clear
            // diagnostic.)
            for name in export.tables.keys() {
                if !TABLES.contains(&name.as_str()) {
                    return Err(DataError::Export(format!(
                        "snapshot contains unknown table {name:?}: this binary's \
                         schema predates the snapshot — refusing to restore \
                         (would silently discard {name:?})"
                    )));
                }
            }

            let tx = conn.transaction()?;

            // The emptiness guard below iterates TABLES; assert it
            // covers the live schema so a future table can't slip a
            // populated store past the "restore targets empty" check.
            assert_table_list_covers_schema(&tx)?;

            // Restore-only: never half-overwrite or silently merge.
            for &table in TABLES {
                let n: i64 =
                    tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                if n > 0 {
                    return Err(DataError::Export(format!(
                        "refusing import: table {table} already has {n} row(s) — \
                         restore targets an empty store"
                    )));
                }
            }

            let mut report = ImportReport::default();
            for &table in TABLES {
                let Some(rows) = export.tables.get(table) else {
                    continue;
                };
                if rows.is_empty() {
                    continue;
                }
                let blobs = blob_columns(&tx, table)?;
                for row in rows {
                    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
                    let placeholders = (1..=cols.len())
                        .map(|i| format!("?{i}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let sql = format!(
                        "INSERT INTO {table} ({}) VALUES ({placeholders})",
                        cols.join(", ")
                    );
                    let bound: Vec<Value> = cols
                        .iter()
                        .map(|c| json_to_sql(&row[*c], blobs.contains(*c)))
                        .collect::<Result<Vec<_>>>()?;
                    let mut stmt = tx.prepare_cached(&sql)?;
                    stmt.execute(rusqlite::params_from_iter(bound.iter()))?;
                    *report.inserted.entry(table.to_string()).or_default() += 1;
                }
            }

            tx.commit()?;
            Ok(report)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EntityType, NewNote, NewProject, NewReference, ProjectStatus};

    #[test]
    fn hex_round_trips_including_uuid_bytes() {
        let id = uuid::Uuid::now_v7();
        let h = to_hex(id.as_bytes());
        assert_eq!(h.len(), 32);
        assert_eq!(from_hex(&h).unwrap(), id.as_bytes());
        assert!(from_hex("abc").is_err(), "odd length rejected");
        assert!(from_hex("zz").is_err(), "non-hex rejected");
    }

    async fn seeded() -> DataStore {
        let s = DataStore::open_in_memory().await.unwrap();
        let p = s
            .insert_project(NewProject {
                name: "Thesis".into(),
                status: ProjectStatus::Active,
                description: "d".into(),
                open_questions: vec!["why?".into()],
            })
            .await
            .unwrap();
        let n = s
            .insert_note(NewNote {
                title: "Lit note".into(),
                content: "body".into(),
                project_id: Some(p.id),
            })
            .await
            .unwrap();
        let r = s
            .insert_reference(NewReference {
                title: "Attention".into(),
                authors: vec!["Vaswani".into()],
                year: Some(2017),
                venue: None,
                doi: None,
                citekey: "vaswani2017".into(),
                abstract_text: None,
                pdf_path: None,
                reading_progress: Some(0.5),
                annotations: vec![],
                project_id: Some(p.id),
            })
            .await
            .unwrap();
        s.add_relation(n.id, EntityType::Note, r.id, EntityType::Reference, "scaffolded_from")
            .await
            .unwrap();
        s.add_tag(n.id, EntityType::Note, "ml").await.unwrap();
        s
    }

    #[tokio::test]
    async fn export_import_round_trips_byte_for_byte() {
        let src = seeded().await;
        let snap = src.export_all().await.unwrap();
        assert_eq!(snap.version, EXPORT_VERSION);
        assert!(snap.row_count() >= 5, "project+note+ref+relation+tag");

        let dst = DataStore::open_in_memory().await.unwrap();
        let report = dst.import_all(snap.clone()).await.unwrap();
        assert_eq!(report.total(), snap.row_count());

        // The whole point: a re-export of the restored store is
        // identical (ids, timestamps, FK links, JSON-array columns).
        let snap2 = dst.export_all().await.unwrap();
        assert_eq!(snap.tables, snap2.tables);

        // And the typed read path sees the restored entities, with the
        // original ids and the FK intact.
        let notes = dst
            .list_notes(crate::models::ListNotes::default())
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        let src_notes = src
            .list_notes(crate::models::ListNotes::default())
            .await
            .unwrap();
        assert_eq!(notes[0].id, src_notes[0].id, "id preserved across restore");
        assert!(notes[0].project_id.is_some(), "FK preserved");
        // FTS triggers fired on the restored insert.
        let hits = dst.search_notes("Lit", 5).await.unwrap();
        assert_eq!(hits.len(), 1, "FTS rebuilt by trigger on restore");
    }

    #[tokio::test]
    async fn import_refuses_non_empty_store() {
        let snap = seeded().await.export_all().await.unwrap();
        let dst = seeded().await; // already populated
        let err = dst.import_all(snap).await.unwrap_err();
        assert!(matches!(err, DataError::Export(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn import_rejects_unknown_version() {
        let mut snap = seeded().await.export_all().await.unwrap();
        snap.version = EXPORT_VERSION + 1;
        let dst = DataStore::open_in_memory().await.unwrap();
        let err = dst.import_all(snap).await.unwrap_err();
        assert!(matches!(err, DataError::Export(m) if m.contains("version")));
    }

    #[tokio::test]
    async fn import_rejects_snapshot_with_unknown_table() {
        // A snapshot from a future schema must not silently drop the
        // table this binary doesn't know — it must error.
        let dst = DataStore::open_in_memory().await.unwrap();
        let mut tables: BTreeMap<String, Vec<RowMap>> = BTreeMap::new();
        let mut row = RowMap::new();
        row.insert("id".into(), serde_json::json!("00"));
        tables.insert("future_widgets".into(), vec![row]);
        let snap = StoreExport {
            version: EXPORT_VERSION,
            exported_at: Utc::now(),
            tables,
        };
        let err = dst.import_all(snap).await.unwrap_err();
        assert!(
            matches!(err, DataError::Export(ref m) if m.contains("unknown table")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn export_fails_when_schema_has_unlisted_table() {
        // Simulate a future migration adding a table TABLES doesn't
        // list: a "successful" backup that silently omits it is exactly
        // the failure mode this guard prevents.
        let s = DataStore::open_in_memory().await.unwrap();
        s.with_conn(|conn| {
            conn.execute("CREATE TABLE future_widgets (id BLOB PRIMARY KEY)", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let err = s.export_all().await.unwrap_err();
        assert!(
            matches!(err, DataError::Export(ref m)
                if m.contains("stale") && m.contains("future_widgets")),
            "got {err:?}"
        );
    }
}
