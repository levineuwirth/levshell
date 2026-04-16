//! Provenance API: read/write rows in the `sync_metadata` table.
//!
//! `sync_metadata` is keyed by `(entity_id, entity_type, provider)`. A native
//! entity has zero matching rows; an entity imported by a sync adapter has one
//! row per provider that touches it. Callers reconstruct a [`DataSource`] in
//! memory by reading the entity-id row out of this table when they need the
//! provenance for a given entity.
//!
//! [`DataSource`]: crate::models::DataSource

use rusqlite::{params, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::models::{EntityType, SyncDirection, SyncMetadata};
use crate::store::DataStore;

const SYNC_METADATA_COLUMNS: &str =
    "entity_id, entity_type, provider, external_id, last_synced_at, sync_direction, sync_hash";

fn row_to_sync_metadata(row: &Row<'_>) -> rusqlite::Result<SyncMetadata> {
    let entity_type_str: String = row.get("entity_type")?;
    let entity_type = EntityType::from_db(&entity_type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let direction_str: String = row.get("sync_direction")?;
    let sync_direction = SyncDirection::from_db(&direction_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(SyncMetadata {
        entity_id: row.get("entity_id")?,
        entity_type,
        provider: row.get("provider")?,
        external_id: row.get("external_id")?,
        last_synced_at: row.get("last_synced_at")?,
        sync_direction,
        sync_hash: row.get("sync_hash")?,
    })
}

impl DataStore {
    pub async fn set_sync_metadata(&self, meta: SyncMetadata) -> Result<()> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO sync_metadata ({SYNC_METADATA_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (entity_id, entity_type, provider) DO UPDATE SET \
                    external_id = excluded.external_id, \
                    last_synced_at = excluded.last_synced_at, \
                    sync_direction = excluded.sync_direction, \
                    sync_hash = excluded.sync_hash"
            ))?;
            stmt.execute(params![
                meta.entity_id,
                meta.entity_type.as_str(),
                meta.provider,
                meta.external_id,
                meta.last_synced_at,
                meta.sync_direction.as_str(),
                meta.sync_hash,
            ])?;
            Ok(())
        })
        .await
    }

    pub async fn get_sync_metadata(
        &self,
        entity_id: Uuid,
        entity_type: EntityType,
        provider: impl Into<String>,
    ) -> Result<Option<SyncMetadata>> {
        let provider = provider.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SYNC_METADATA_COLUMNS} FROM sync_metadata \
                 WHERE entity_id = ?1 AND entity_type = ?2 AND provider = ?3"
            ))?;
            match stmt.query_row(
                params![entity_id, entity_type.as_str(), provider],
                row_to_sync_metadata,
            ) {
                Ok(m) => Ok(Some(m)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn clear_sync_metadata(
        &self,
        entity_id: Uuid,
        entity_type: EntityType,
        provider: impl Into<String>,
    ) -> Result<bool> {
        let provider = provider.into();
        self.with_conn(move |conn| {
            let n = conn.execute(
                "DELETE FROM sync_metadata \
                 WHERE entity_id = ?1 AND entity_type = ?2 AND provider = ?3",
                params![entity_id, entity_type.as_str(), provider],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// List every sync_metadata row for a given provider. Used by adapters
    /// during a full-sync pass to detect entities whose external source has
    /// disappeared (e.g. a deleted Obsidian vault file).
    pub async fn list_sync_metadata_by_provider(
        &self,
        provider: impl Into<String>,
    ) -> Result<Vec<SyncMetadata>> {
        let provider = provider.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SYNC_METADATA_COLUMNS} FROM sync_metadata \
                 WHERE provider = ?1"
            ))?;
            let rows = stmt
                .query_map(params![provider], row_to_sync_metadata)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Look up a single sync_metadata row by `(provider, external_id)`.
    /// Used by adapters to decide whether an external source path has
    /// already been synced (update) or is new (insert).
    pub async fn find_sync_metadata_by_external_id(
        &self,
        provider: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Result<Option<SyncMetadata>> {
        let provider = provider.into();
        let external_id = external_id.into();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {SYNC_METADATA_COLUMNS} FROM sync_metadata \
                 WHERE provider = ?1 AND external_id = ?2"
            ))?;
            match stmt.query_row(params![provider, external_id], row_to_sync_metadata) {
                Ok(m) => Ok(Some(m)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }
}
