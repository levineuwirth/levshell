//! CRUD operations for the `experiments` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{Experiment, ExperimentPatch, ExperimentStatus, ListExperiments, NewExperiment};
use crate::store::DataStore;

const EXPERIMENT_COLUMNS: &str =
    "id, name, project_id, hypothesis, status, host, git_hash, \
     config, metrics, notes, started_at, completed_at, created_at, updated_at";

fn row_to_experiment(row: &Row<'_>) -> rusqlite::Result<Experiment> {
    let status_str: String = row.get("status")?;
    let status = ExperimentStatus::from_db(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let config: Option<serde_json::Value> = match row.get::<_, Option<String>>("config")? {
        None => None,
        Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
        })?),
    };

    let metrics_str: String = row.get("metrics")?;
    let metrics: serde_json::Value = serde_json::from_str(&metrics_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Experiment {
        id: row.get("id")?,
        name: row.get("name")?,
        project_id: row.get("project_id")?,
        hypothesis: row.get("hypothesis")?,
        status,
        host: row.get("host")?,
        git_hash: row.get("git_hash")?,
        config,
        metrics,
        notes: row.get("notes")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_experiment(&self, new: NewExperiment) -> Result<Experiment> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();
            let config_json = new.config.map(|c| serde_json::to_string(&c)).transpose()?;
            let metrics_json = serde_json::to_string(&serde_json::json!({}))?;

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO experiments ({EXPERIMENT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?12) \
                 RETURNING {EXPERIMENT_COLUMNS}"
            ))?;
            let exp = stmt.query_row(
                params![
                    id,
                    new.name,
                    new.project_id,
                    new.hypothesis,
                    new.status.as_str(),
                    new.host,
                    new.git_hash,
                    config_json,
                    metrics_json,
                    new.notes,
                    now,
                    now,
                ],
                row_to_experiment,
            )?;
            Ok(exp)
        })
        .await
    }

    pub async fn get_experiment(&self, id: Uuid) -> Result<Option<Experiment>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {EXPERIMENT_COLUMNS} FROM experiments WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_experiment) {
                Ok(e) => Ok(Some(e)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_experiments(&self, params: ListExperiments) -> Result<Vec<Experiment>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {EXPERIMENT_COLUMNS} FROM experiments WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(project_id) = params.project_id {
                sql.push_str(" AND project_id = ?");
                binds.push(Value::Blob(project_id.as_bytes().to_vec()));
            }
            if let Some(status) = params.status {
                sql.push_str(" AND status = ?");
                binds.push(Value::Text(status.as_str().to_string()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'experiment' AND tag = ?)",
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
                .query_map(params_from_iter(binds.iter()), row_to_experiment)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_experiment(&self, id: Uuid, patch: ExperimentPatch) -> Result<Experiment> {
        self.with_conn(move |conn| {
            let now = Utc::now();
            let config_json = match &patch.config {
                None => None,
                Some(None) => Some(None),
                Some(Some(v)) => Some(Some(serde_json::to_string(v)?)),
            };
            let metrics_json = patch.metrics.map(|m| serde_json::to_string(&m)).transpose()?;

            let (set_hypothesis, hypothesis_val) = match patch.hypothesis {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_host, host_val) = match patch.host {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_git_hash, git_hash_val) = match patch.git_hash {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_config, config_val) = match config_json {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_notes, notes_val) = match patch.notes {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_started, started_val) = match patch.started_at {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_completed, completed_val) = match patch.completed_at {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE experiments SET \
                    name = COALESCE(?2, name), \
                    hypothesis = CASE WHEN ?3 THEN ?4 ELSE hypothesis END, \
                    status = COALESCE(?5, status), \
                    host = CASE WHEN ?6 THEN ?7 ELSE host END, \
                    git_hash = CASE WHEN ?8 THEN ?9 ELSE git_hash END, \
                    config = CASE WHEN ?10 THEN ?11 ELSE config END, \
                    metrics = COALESCE(?12, metrics), \
                    notes = CASE WHEN ?13 THEN ?14 ELSE notes END, \
                    started_at = CASE WHEN ?15 THEN ?16 ELSE started_at END, \
                    completed_at = CASE WHEN ?17 THEN ?18 ELSE completed_at END, \
                    updated_at = ?19 \
                 WHERE id = ?1 \
                 RETURNING {EXPERIMENT_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![
                    id,
                    patch.name,
                    set_hypothesis, hypothesis_val,
                    patch.status.map(|s| s.as_str()),
                    set_host, host_val,
                    set_git_hash, git_hash_val,
                    set_config, config_val,
                    metrics_json,
                    set_notes, notes_val,
                    set_started, started_val,
                    set_completed, completed_val,
                    now,
                ],
                row_to_experiment,
            ) {
                Ok(e) => Ok(e),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_experiment(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM experiments WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
