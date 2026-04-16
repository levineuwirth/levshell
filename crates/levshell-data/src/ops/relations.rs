//! CRUD operations on the polymorphic `entity_relations` table.
//!
//! `entity_relations` encodes directed edges between any two entities
//! in the unified data model. Each row is one edge of a typed
//! `kind` — `"wiki_link"`, `"cites"`, `"derived_from"`, etc. The
//! schema's primary key on
//! `(source_id, source_type, target_id, target_type, relation_kind)`
//! means `add_relation` is idempotent: repeating an add for an edge
//! that already exists is a no-op at the DB level (INSERT OR IGNORE).
//!
//! Sync adapters use this table to populate the knowledge graph
//! (§3.3.3 of the spec): the Obsidian adapter translates
//! `[[wiki-links]]` into rows, Zotero extracts citation edges, etc.

use chrono::Utc;
use rusqlite::{params, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::models::{EntityType, Relation};
use crate::store::DataStore;

const RELATION_COLUMNS: &str =
    "source_id, source_type, target_id, target_type, relation_kind, created_at";

fn row_to_relation(row: &Row<'_>) -> rusqlite::Result<Relation> {
    let source_type_str: String = row.get("source_type")?;
    let source_type = EntityType::from_db(&source_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let target_type_str: String = row.get("target_type")?;
    let target_type = EntityType::from_db(&target_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Relation {
        source_id: row.get("source_id")?,
        source_type,
        target_id: row.get("target_id")?,
        target_type,
        kind: row.get("relation_kind")?,
        created_at: row.get("created_at")?,
    })
}

impl DataStore {
    /// Add an edge from one entity to another, tagged by `kind`.
    /// Idempotent: re-adding an existing `(source, target, kind)` is
    /// silently ignored. Returns `true` iff a new row was inserted.
    pub async fn add_relation(
        &self,
        source_id: Uuid,
        source_type: EntityType,
        target_id: Uuid,
        target_type: EntityType,
        kind: impl Into<String>,
    ) -> Result<bool> {
        let kind = kind.into();
        self.with_conn(move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO entity_relations \
                 (source_id, source_type, target_id, target_type, relation_kind, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    source_id,
                    source_type.as_str(),
                    target_id,
                    target_type.as_str(),
                    kind,
                    Utc::now(),
                ],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// Remove a single edge by its full composite key.
    pub async fn remove_relation(
        &self,
        source_id: Uuid,
        source_type: EntityType,
        target_id: Uuid,
        target_type: EntityType,
        kind: impl Into<String>,
    ) -> Result<bool> {
        let kind = kind.into();
        self.with_conn(move |conn| {
            let n = conn.execute(
                "DELETE FROM entity_relations \
                 WHERE source_id = ?1 AND source_type = ?2 \
                   AND target_id = ?3 AND target_type = ?4 \
                   AND relation_kind = ?5",
                params![
                    source_id,
                    source_type.as_str(),
                    target_id,
                    target_type.as_str(),
                    kind,
                ],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// Every outgoing edge from `(source_id, source_type)` regardless
    /// of kind. Order is (relation_kind asc, target_id asc) so callers
    /// can walk the result deterministically.
    pub async fn list_relations_from(
        &self,
        source_id: Uuid,
        source_type: EntityType,
    ) -> Result<Vec<Relation>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {RELATION_COLUMNS} FROM entity_relations \
                 WHERE source_id = ?1 AND source_type = ?2 \
                 ORDER BY relation_kind, target_id"
            ))?;
            let rows = stmt
                .query_map(params![source_id, source_type.as_str()], row_to_relation)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Every incoming edge to `(target_id, target_type)` (backlinks).
    /// Uses `idx_relations_target` for efficient lookup.
    pub async fn list_relations_to(
        &self,
        target_id: Uuid,
        target_type: EntityType,
    ) -> Result<Vec<Relation>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {RELATION_COLUMNS} FROM entity_relations \
                 WHERE target_id = ?1 AND target_type = ?2 \
                 ORDER BY relation_kind, source_id"
            ))?;
            let rows = stmt
                .query_map(params![target_id, target_type.as_str()], row_to_relation)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Delete every edge from `(source_id, source_type)` tagged with
    /// `kind`. Used by sync adapters to reset a note's outgoing edges
    /// of a particular kind before re-populating them from fresh
    /// content — mirrors the "diff then apply" pattern the tag
    /// replacement uses.
    pub async fn clear_relations_from(
        &self,
        source_id: Uuid,
        source_type: EntityType,
        kind: impl Into<String>,
    ) -> Result<usize> {
        let kind = kind.into();
        self.with_conn(move |conn| {
            let n = conn.execute(
                "DELETE FROM entity_relations \
                 WHERE source_id = ?1 AND source_type = ?2 \
                   AND relation_kind = ?3",
                params![source_id, source_type.as_str(), kind],
            )?;
            Ok(n)
        })
        .await
    }
}
