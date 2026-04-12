//! Polymorphic tag operations on the `entity_tags` table.

use rusqlite::params;
use uuid::Uuid;

use crate::error::Result;
use crate::models::EntityType;
use crate::store::DataStore;

impl DataStore {
    pub async fn add_tag(
        &self,
        entity_id: Uuid,
        entity_type: EntityType,
        tag: impl Into<String>,
    ) -> Result<()> {
        let tag = tag.into();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO entity_tags (entity_id, entity_type, tag) \
                 VALUES (?1, ?2, ?3)",
                params![entity_id, entity_type.as_str(), tag],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn remove_tag(
        &self,
        entity_id: Uuid,
        entity_type: EntityType,
        tag: impl Into<String>,
    ) -> Result<bool> {
        let tag = tag.into();
        self.with_conn(move |conn| {
            let n = conn.execute(
                "DELETE FROM entity_tags \
                 WHERE entity_id = ?1 AND entity_type = ?2 AND tag = ?3",
                params![entity_id, entity_type.as_str(), tag],
            )?;
            Ok(n > 0)
        })
        .await
    }

    pub async fn get_tags(
        &self,
        entity_id: Uuid,
        entity_type: EntityType,
    ) -> Result<Vec<String>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT tag FROM entity_tags \
                 WHERE entity_id = ?1 AND entity_type = ?2 \
                 ORDER BY tag",
            )?;
            let rows = stmt
                .query_map(params![entity_id, entity_type.as_str()], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn find_by_tag(
        &self,
        entity_type: EntityType,
        tag: impl Into<String>,
    ) -> Result<Vec<Uuid>> {
        let tag = tag.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT entity_id FROM entity_tags \
                 WHERE entity_type = ?1 AND tag = ?2 \
                 ORDER BY entity_id",
            )?;
            let rows = stmt
                .query_map(params![entity_type.as_str(), tag], |row| {
                    row.get::<_, Uuid>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}
