//! Context-triggered Anki review (spec §2.9.6 — "When switching to a
//! workspace associated with a topic that has due Anki cards, suggest
//! reviewing those cards first").
//!
//! On `WorkspaceChanged`, resolve the workspace to a project via the
//! registry's static `workspace_names` mapping, count that project's due
//! flashcards, and — if any are due and the project hasn't been nudged
//! recently — publish an `Event::NudgeDelivered`. That reuses the M1.5
//! nudge surface (NotificationsModule → toast + Freedesktop): no new
//! delivery plumbing.
//!
//! A per-project cooldown keeps this a gentle once-in-a-while suggestion
//! rather than a nag on every workspace hop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use levshell_core::{Event, EventBus, EventKind, Module, ModuleResult};
use levshell_data::{DataStore, ListFlashcards};
use levshell_projects::ProjectRegistry;
use uuid::Uuid;

const MODULE_NAME: &str = "anki-review";
/// Don't re-suggest the same project's reviews more than once per hour,
/// however often the user bounces through its workspace.
const COOLDOWN: Duration = Duration::from_secs(3600);
const DUE_QUERY_LIMIT: u32 = 10_000;

pub struct AnkiReviewModule {
    store: DataStore,
    bus: EventBus,
    projects: Option<ProjectRegistry>,
    last_nudged: HashMap<Uuid, Instant>,
    cooldown: Duration,
}

impl AnkiReviewModule {
    pub fn new(store: DataStore, bus: EventBus, projects: Option<ProjectRegistry>) -> Self {
        Self {
            store,
            bus,
            projects,
            last_nudged: HashMap::new(),
            cooldown: COOLDOWN,
        }
    }

    #[cfg(test)]
    fn with_cooldown(mut self, d: Duration) -> Self {
        self.cooldown = d;
        self
    }

    async fn on_workspace(&mut self, workspace: &str) {
        let Some(registry) = self.projects.as_ref() else {
            return;
        };
        let Some(entry) = registry.find_by_workspace(workspace).await else {
            return;
        };
        let project_id = entry.project.id;
        let project_name = entry.project.name.clone();

        let due = match self
            .store
            .list_flashcards(ListFlashcards {
                project_id: Some(project_id),
                due_before: Some(Utc::now()),
                limit: Some(DUE_QUERY_LIMIT),
                ..Default::default()
            })
            .await
        {
            Ok(f) => f.len(),
            Err(e) => {
                tracing::warn!(error = %e, "anki-review: due query failed");
                return;
            }
        };
        if due == 0 {
            return;
        }

        let now = Instant::now();
        if let Some(prev) = self.last_nudged.get(&project_id) {
            if now.duration_since(*prev) < self.cooldown {
                tracing::debug!(
                    project = %project_name,
                    "anki-review: within cooldown; skipping"
                );
                return;
            }
        }
        self.last_nudged.insert(project_id, now);

        let card = if due == 1 { "card" } else { "cards" };
        self.bus.publish(Event::NudgeDelivered {
            project_id,
            kind: "anki_review".into(),
            title: format!("{due} {card} due — review {project_name} first"),
        });
        tracing::info!(
            project = %project_name,
            due,
            "anki-review: suggested review on workspace entry"
        );
    }
}

#[async_trait]
impl Module for AnkiReviewModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WorkspaceChanged]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if let Event::WorkspaceChanged { name, .. } = event {
            self.on_workspace(name).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_core::EventKind;
    use levshell_data::NewFlashcard;
    use levshell_projects::{ProjectFile, ProjectRegistry};

    fn due_card(project_id: Uuid) -> NewFlashcard {
        NewFlashcard {
            front: "q".into(),
            back: "a".into(),
            linked_note_id: None,
            linked_ref_id: None,
            project_id: Some(project_id),
            interval_days: 1.0,
            ease_factor: 2.5,
            due_at: Utc::now() - chrono::Duration::days(1),
        }
    }

    fn project_file(name: &str, workspace: &str) -> ProjectFile {
        ProjectFile {
            name: name.into(),
            status: None,
            description: None,
            open_questions: Vec::new(),
            tags: Vec::new(),
            git_repos: Vec::new(),
            ssh_hosts: Vec::new(),
            workspace_names: vec![workspace.into()],
            accent_color: None,
        }
    }

    /// `upsert_from_file` both creates the project row and indexes the
    /// workspace mapping, so there's nothing else to set up.
    async fn setup() -> (DataStore, EventBus, ProjectRegistry) {
        let store = DataStore::open_in_memory().await.unwrap();
        let bus = EventBus::new();
        let registry = ProjectRegistry::empty(store.clone(), bus.clone());
        registry
            .upsert_from_file(project_file("llm-alignment", "research"))
            .await
            .unwrap();
        (store, bus, registry)
    }

    #[tokio::test]
    async fn nudges_when_due_cards_in_entered_workspace() {
        let (store, bus, registry) = setup().await;
        // The project id the registry assigned (from its own upsert).
        let entry = registry.find_by_workspace("research").await.unwrap();
        store
            .insert_flashcard(due_card(entry.project.id))
            .await
            .unwrap();

        let mut rx = bus.subscribe("t", [EventKind::NudgeDelivered], 4);
        let mut m = AnkiReviewModule::new(store, bus.clone(), Some(registry));

        m.on_event(&Event::WorkspaceChanged {
            name: "research".into(),
            focused_window: None,
        })
        .await
        .unwrap();

        match rx.try_recv().expect("a nudge") {
            Event::NudgeDelivered { kind, title, .. } => {
                assert_eq!(kind, "anki_review");
                assert!(title.contains("review llm-alignment"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cooldown_suppresses_repeat_and_unmapped_ws_is_silent() {
        let (store, bus, registry) = setup().await;
        let entry = registry.find_by_workspace("research").await.unwrap();
        store
            .insert_flashcard(due_card(entry.project.id))
            .await
            .unwrap();
        let mut rx = bus.subscribe("t", [EventKind::NudgeDelivered], 4);
        let mut m = AnkiReviewModule::new(store, bus.clone(), Some(registry))
            .with_cooldown(Duration::from_secs(3600));

        let enter = || Event::WorkspaceChanged {
            name: "research".into(),
            focused_window: None,
        };
        m.on_event(&enter()).await.unwrap();
        assert!(rx.try_recv().is_ok(), "first entry nudges");
        m.on_event(&enter()).await.unwrap();
        assert!(rx.try_recv().is_err(), "second entry within cooldown is silent");

        // A workspace with no project mapping never nudges.
        m.on_event(&Event::WorkspaceChanged {
            name: "scratch".into(),
            focused_window: None,
        })
        .await
        .unwrap();
        assert!(rx.try_recv().is_err());
    }
}
