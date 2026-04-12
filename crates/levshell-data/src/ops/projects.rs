//! CRUD operations for the `projects` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{ListProjects, NewProject, Project, ProjectPatch, ProjectStatus};
use crate::store::DataStore;

const PROJECT_COLUMNS: &str =
    "id, name, status, description, open_questions, created_at, updated_at";

fn row_to_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    let open_questions_json: String = row.get("open_questions")?;
    let open_questions: Vec<String> = serde_json::from_str(&open_questions_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;

    let status_str: String = row.get("status")?;
    let status = ProjectStatus::from_db(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        status,
        description: row.get("description")?,
        open_questions,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_project(&self, new: NewProject) -> Result<Project> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();
            let open_questions_json = serde_json::to_string(&new.open_questions)?;

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO projects ({PROJECT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 RETURNING {PROJECT_COLUMNS}"
            ))?;
            let project = stmt.query_row(
                params![
                    id,
                    new.name,
                    new.status.as_str(),
                    new.description,
                    open_questions_json,
                    now,
                    now,
                ],
                row_to_project,
            )?;
            Ok(project)
        })
        .await
    }

    pub async fn get_project(&self, id: Uuid) -> Result<Option<Project>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_project) {
                Ok(p) => Ok(Some(p)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_projects(&self, params: ListProjects) -> Result<Vec<Project>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(status) = params.status {
                sql.push_str(" AND status = ?");
                binds.push(Value::Text(status.as_str().to_string()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'project' AND tag = ?)",
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
                .query_map(params_from_iter(binds.iter()), row_to_project)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_project(&self, id: Uuid, patch: ProjectPatch) -> Result<Project> {
        self.with_conn(move |conn| {
            let open_questions_json = match patch.open_questions {
                Some(qs) => Some(serde_json::to_string(&qs)?),
                None => None,
            };
            let now = Utc::now();

            let mut stmt = conn.prepare_cached(&format!(
                "UPDATE projects SET \
                    name = COALESCE(?2, name), \
                    status = COALESCE(?3, status), \
                    description = COALESCE(?4, description), \
                    open_questions = COALESCE(?5, open_questions), \
                    updated_at = ?6 \
                 WHERE id = ?1 \
                 RETURNING {PROJECT_COLUMNS}"
            ))?;
            match stmt.query_row(
                params![
                    id,
                    patch.name,
                    patch.status.map(|s| s.as_str()),
                    patch.description,
                    open_questions_json,
                    now,
                ],
                row_to_project,
            ) {
                Ok(p) => Ok(p),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_project(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
