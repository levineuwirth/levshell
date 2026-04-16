//! CRUD operations for the `tasks` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{ListTasks, NewTask, Task, TaskPatch, TaskPriority, TaskStatus};
use crate::store::DataStore;

const TASK_COLUMNS: &str =
    "id, title, description, status, priority, due_at, project_id, created_at, updated_at";

fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    let status_str: String = row.get("status")?;
    let status = TaskStatus::from_db(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let priority: Option<TaskPriority> = match row.get::<_, Option<String>>("priority")? {
        None => None,
        Some(s) => Some(TaskPriority::from_db(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?),
    };

    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row.get("description")?,
        status,
        priority,
        due_at: row.get("due_at")?,
        project_id: row.get("project_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_task(&self, new: NewTask) -> Result<Task> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO tasks ({TASK_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 RETURNING {TASK_COLUMNS}"
            ))?;
            let task = stmt.query_row(
                params![
                    id,
                    new.title,
                    new.description,
                    new.status.as_str(),
                    new.priority.map(|p| p.as_str()),
                    new.due_at,
                    new.project_id,
                    now,
                    now,
                ],
                row_to_task,
            )?;
            Ok(task)
        })
        .await
    }

    pub async fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_task) {
                Ok(t) => Ok(Some(t)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_tasks(&self, params: ListTasks) -> Result<Vec<Task>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1=1");
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
                     WHERE entity_type = 'task' AND tag = ?)",
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
                .query_map(params_from_iter(binds.iter()), row_to_task)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_task(&self, id: Uuid, patch: TaskPatch) -> Result<Task> {
        self.with_conn(move |conn| {
            let now = Utc::now();

            let (set_desc, desc_val) = match patch.description {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_priority, priority_val): (bool, Option<&str>) = match patch.priority {
                None => (false, None),
                Some(v) => (true, v.map(|p| p.as_str())),
            };
            let (set_due, due_val) = match patch.due_at {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_project, project_val) = match patch.project_id {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE tasks SET \
                    title = COALESCE(?2, title), \
                    description = CASE WHEN ?3 THEN ?4 ELSE description END, \
                    status = COALESCE(?5, status), \
                    priority = CASE WHEN ?6 THEN ?7 ELSE priority END, \
                    due_at = CASE WHEN ?8 THEN ?9 ELSE due_at END, \
                    project_id = CASE WHEN ?10 THEN ?11 ELSE project_id END, \
                    updated_at = ?12 \
                 WHERE id = ?1 \
                 RETURNING {TASK_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![
                    id,
                    patch.title,
                    set_desc, desc_val,
                    patch.status.map(|s| s.as_str()),
                    set_priority, priority_val,
                    set_due, due_val,
                    set_project, project_val,
                    now,
                ],
                row_to_task,
            ) {
                Ok(t) => Ok(t),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_task(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
