//! Project pulse + deadline tracker (spec §2.9.4, §2.9.12).
//!
//! §2.9.4: "which projects have had activity today and which have been
//! dormant beyond a configurable threshold. Clicking expands a
//! dashboard: last touched, hours logged, open questions."
//! §2.9.12: "Upcoming deadlines … colour-coded by urgency" — sourced
//! from the unified model's `tasks` (with a due date) and `events`
//! (calendar), so a CalDAV-synced deadline and a native task surface
//! through the same widget.
//!
//! Render side: polls the project registry runtime + the data store,
//! publishes a `project-pulse` widget. No external tools touched.
//!
//! State: `{ active_today, dormant, projects: [...], deadlines: [...] }`.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use levshell_core::{Module, ModuleResult, WidgetDescriptor};
use levshell_data::{DataStore, ListEvents, ListTasks, TaskStatus};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};
use levshell_projects::ProjectRegistry;

pub const PROJECT_PULSE_WIDGET_ID: &str = "project-pulse";
pub const PROJECT_PULSE_WIDGET_TYPE: &str = "project_pulse";
const MODULE_NAME: &str = "project-pulse";

const TICK: StdDuration = StdDuration::from_secs(120);
/// A project untouched for longer than this reads as "dormant" (spec
/// §2.9.4 "dormant beyond a configurable threshold" — config is a later
/// refinement; 7 days is a sane default).
const DORMANT_DAYS: i64 = 7;
/// Deadline look-ahead window.
const DEADLINE_DAYS: i64 = 30;
const MAX_DEADLINES: usize = 10;

pub struct ProjectPulseModule {
    registry: Option<ProjectRegistry>,
    store: DataStore,
    publisher: WidgetPublisher,
}

impl ProjectPulseModule {
    pub fn new(
        registry: Option<ProjectRegistry>,
        store: DataStore,
        publisher: WidgetPublisher,
    ) -> Self {
        Self {
            registry,
            store,
            publisher,
        }
    }

    async fn deadlines(&self) -> Vec<serde_json::Value> {
        let now = Utc::now();
        let horizon = now + Duration::days(DEADLINE_DAYS);
        let mut out: Vec<(chrono::DateTime<Utc>, serde_json::Value)> = Vec::new();

        // Tasks with a due date in the window that aren't finished.
        // `ListTasks` has no due filter, so bound the scan and filter
        // the date range in memory.
        if let Ok(tasks) = self
            .store
            .list_tasks(ListTasks {
                limit: Some(500),
                ..Default::default()
            })
            .await
        {
            for t in tasks {
                if matches!(t.status, TaskStatus::Done | TaskStatus::Cancelled) {
                    continue;
                }
                if let Some(due) = t.due_at {
                    if due > horizon {
                        continue;
                    }
                    out.push((
                        due,
                        serde_json::json!({
                            "title": t.title,
                            "due": due.to_rfc3339(),
                            "kind": "task",
                            "overdue": due < now,
                        }),
                    ));
                }
            }
        }

        // Calendar events starting within the window.
        if let Ok(events) = self
            .store
            .list_events(ListEvents {
                after: Some(now),
                before: Some(horizon),
                limit: Some(100),
                ..Default::default()
            })
            .await
        {
            for e in events {
                out.push((
                    e.start_at,
                    serde_json::json!({
                        "title": e.title,
                        "due": e.start_at.to_rfc3339(),
                        "kind": "event",
                        "overdue": false,
                    }),
                ));
            }
        }

        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.into_iter()
            .take(MAX_DEADLINES)
            .map(|(_, v)| v)
            .collect()
    }

    async fn refresh(&self) {
        let now = Utc::now();
        let today = now.date_naive();
        let dormant_cutoff = now - Duration::days(DORMANT_DAYS);

        let entries = match self.registry.as_ref() {
            Some(r) => r.list().await,
            None => Vec::new(),
        };

        let mut active_today = 0usize;
        let mut dormant = 0usize;
        let projects: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let last = e.runtime.last_active_at;
                let is_active_today =
                    last.map(|t| t.date_naive() == today).unwrap_or(false);
                let is_dormant = last.map(|t| t < dormant_cutoff).unwrap_or(true);
                if is_active_today {
                    active_today += 1;
                }
                if is_dormant {
                    dormant += 1;
                }
                serde_json::json!({
                    "name": e.project.name,
                    "status": e.project.status.as_str(),
                    "active_today": is_active_today,
                    "dormant": is_dormant,
                    "last_active": last.map(|t| t.to_rfc3339()),
                    "focus_secs": e.runtime.accumulated_focus_time_secs,
                    "open_questions": e.project.open_questions.len(),
                })
            })
            .collect();

        let deadlines = self.deadlines().await;

        let update = WidgetUpdate {
            widget_id: PROJECT_PULSE_WIDGET_ID.into(),
            widget_type: PROJECT_PULSE_WIDGET_TYPE.into(),
            state: serde_json::json!({
                "active_today": active_today,
                "dormant": dormant,
                "projects": projects,
                "deadlines": deadlines,
            }),
            status: WidgetStatus::Normal,
            escalation: Default::default(),
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "project-pulse: publish drop");
        }
    }
}

#[async_trait]
impl Module for ProjectPulseModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: PROJECT_PULSE_WIDGET_ID.into(),
            widget_type: PROJECT_PULSE_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(TICK)
    }

    async fn start(&mut self) -> ModuleResult<()> {
        self.refresh().await;
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.refresh().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_data::{NewEvent, NewTask};
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::{duplex, AsyncReadExt};

    async fn first_state(store: DataStore) -> serde_json::Value {
        let (a, mut b) = duplex(8192);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 16);
        let m = ProjectPulseModule::new(None, store, task.publisher);
        m.refresh().await;
        let mut buf = vec![0u8; 8192];
        let n = b.read(&mut buf).await.unwrap();
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(line.lines().next().unwrap()).unwrap();
        v["state"].clone()
    }

    fn task(title: &str, due: chrono::DateTime<Utc>, status: TaskStatus) -> NewTask {
        NewTask {
            title: title.into(),
            description: None,
            status,
            priority: None,
            due_at: Some(due),
            project_id: None,
        }
    }
    fn event(title: &str, start: chrono::DateTime<Utc>) -> NewEvent {
        NewEvent {
            title: title.into(),
            start_at: start,
            end_at: start + Duration::hours(1),
            location: None,
            description: None,
            url: None,
            project_id: None,
            recurrence: None,
            reminders: Vec::new(),
        }
    }

    #[tokio::test]
    async fn deadlines_merge_tasks_and_events_sorted() {
        let store = DataStore::open_in_memory().await.unwrap();
        let now = Utc::now();
        store
            .insert_task(task(
                "submit camera-ready",
                now + Duration::days(2),
                TaskStatus::Pending,
            ))
            .await
            .unwrap();
        store
            .insert_event(event("advisor meeting", now + Duration::days(1)))
            .await
            .unwrap();
        // A done task is excluded.
        store
            .insert_task(task("old thing", now + Duration::days(3), TaskStatus::Done))
            .await
            .unwrap();

        let state = first_state(store).await;
        let dl = state["deadlines"].as_array().unwrap();
        assert_eq!(dl.len(), 2, "task + event, done task excluded");
        // Event (day 1) sorts before task (day 2).
        assert_eq!(dl[0]["title"], "advisor meeting");
        assert_eq!(dl[0]["kind"], "event");
        assert_eq!(dl[1]["title"], "submit camera-ready");
        assert_eq!(dl[1]["kind"], "task");
    }

    #[tokio::test]
    async fn no_registry_yields_empty_projects() {
        let store = DataStore::open_in_memory().await.unwrap();
        let state = first_state(store).await;
        assert_eq!(state["active_today"], 0);
        assert_eq!(state["projects"].as_array().unwrap().len(), 0);
    }
}
