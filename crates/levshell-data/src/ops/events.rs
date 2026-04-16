//! CRUD operations for the `events` table.

use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value, Row};
use uuid::Uuid;

use crate::error::{DataError, Result};
use crate::models::{Event, EventPatch, ListEvents, NewEvent};
use crate::store::DataStore;

const EVENT_COLUMNS: &str =
    "id, title, start_at, end_at, location, description, url, \
     project_id, recurrence, reminders, created_at, updated_at";

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let reminders_json: String = row.get("reminders")?;
    let reminders: Vec<String> = serde_json::from_str(&reminders_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Event {
        id: row.get("id")?,
        title: row.get("title")?,
        start_at: row.get("start_at")?,
        end_at: row.get("end_at")?,
        location: row.get("location")?,
        description: row.get("description")?,
        url: row.get("url")?,
        project_id: row.get("project_id")?,
        recurrence: row.get("recurrence")?,
        reminders,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl DataStore {
    pub async fn insert_event(&self, new: NewEvent) -> Result<Event> {
        self.with_conn(move |conn| {
            let id = Uuid::now_v7();
            let now = Utc::now();
            let reminders_json = serde_json::to_string(&new.reminders)?;

            let mut stmt = conn.prepare_cached(&format!(
                "INSERT INTO events ({EVENT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 RETURNING {EVENT_COLUMNS}"
            ))?;
            let event = stmt.query_row(
                params![
                    id,
                    new.title,
                    new.start_at,
                    new.end_at,
                    new.location,
                    new.description,
                    new.url,
                    new.project_id,
                    new.recurrence,
                    reminders_json,
                    now,
                    now,
                ],
                row_to_event,
            )?;
            Ok(event)
        })
        .await
    }

    pub async fn get_event(&self, id: Uuid) -> Result<Option<Event>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {EVENT_COLUMNS} FROM events WHERE id = ?1"
            ))?;
            match stmt.query_row(params![id], row_to_event) {
                Ok(e) => Ok(Some(e)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn list_events(&self, params: ListEvents) -> Result<Vec<Event>> {
        self.with_conn(move |conn| {
            let mut sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE 1=1");
            let mut binds: Vec<Value> = Vec::new();

            if let Some(project_id) = params.project_id {
                sql.push_str(" AND project_id = ?");
                binds.push(Value::Blob(project_id.as_bytes().to_vec()));
            }
            // rusqlite 0.31's ToSql for DateTime<Utc> uses "%F %T%.f%:z"
            // (space between date and time), not RFC3339's 'T'. For string
            // comparison on TEXT columns to work, the filter string must use
            // the same format as stored rows.
            if let Some(after) = params.after {
                sql.push_str(" AND end_at >= ?");
                binds.push(Value::Text(after.format("%F %T%.f%:z").to_string()));
            }
            if let Some(before) = params.before {
                sql.push_str(" AND start_at <= ?");
                binds.push(Value::Text(before.format("%F %T%.f%:z").to_string()));
            }
            if let Some(tag) = params.tag {
                sql.push_str(
                    " AND id IN (SELECT entity_id FROM entity_tags \
                     WHERE entity_type = 'event' AND tag = ?)",
                );
                binds.push(Value::Text(tag));
            }
            sql.push_str(" ORDER BY start_at ASC");
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
                .query_map(params_from_iter(binds.iter()), row_to_event)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn update_event(&self, id: Uuid, patch: EventPatch) -> Result<Event> {
        self.with_conn(move |conn| {
            let reminders_json = patch.reminders.map(|r| serde_json::to_string(&r)).transpose()?;
            let now = Utc::now();

            let (set_location, location_val) = match patch.location {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_desc, desc_val) = match patch.description {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_url, url_val) = match patch.url {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_project, project_val) = match patch.project_id {
                None => (false, None),
                Some(v) => (true, v),
            };
            let (set_recurrence, recurrence_val) = match patch.recurrence {
                None => (false, None),
                Some(v) => (true, v),
            };

            let sql = format!(
                "UPDATE events SET \
                    title = COALESCE(?2, title), \
                    start_at = COALESCE(?3, start_at), \
                    end_at = COALESCE(?4, end_at), \
                    location = CASE WHEN ?5 THEN ?6 ELSE location END, \
                    description = CASE WHEN ?7 THEN ?8 ELSE description END, \
                    url = CASE WHEN ?9 THEN ?10 ELSE url END, \
                    project_id = CASE WHEN ?11 THEN ?12 ELSE project_id END, \
                    recurrence = CASE WHEN ?13 THEN ?14 ELSE recurrence END, \
                    reminders = COALESCE(?15, reminders), \
                    updated_at = ?16 \
                 WHERE id = ?1 \
                 RETURNING {EVENT_COLUMNS}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            match stmt.query_row(
                params![
                    id,
                    patch.title,
                    patch.start_at,
                    patch.end_at,
                    set_location, location_val,
                    set_desc, desc_val,
                    set_url, url_val,
                    set_project, project_val,
                    set_recurrence, recurrence_val,
                    reminders_json,
                    now,
                ],
                row_to_event,
            ) {
                Ok(e) => Ok(e),
                Err(rusqlite::Error::QueryReturnedNoRows) => Err(DataError::NotFound),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }

    pub async fn delete_event(&self, id: Uuid) -> Result<bool> {
        self.with_conn(move |conn| {
            let n = conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
    }
}
