//! Integration tests for `levshell-data::DataStore`.
//!
//! Each test opens a fresh in-memory or tempfile-backed database, runs the
//! embedded migration, and exercises one slice of the public API. Together
//! they cover the Phase 0 acceptance criteria from spec §6.1 step 0.2:
//! migrations apply, Project + Note round-trip, FTS search returns hits, the
//! polymorphic tag table works across entity types, and the sync_metadata
//! provenance API stores and reads back.

use chrono::{Duration, Utc};
use levshell_data::{
    DataStore, EntityType, EventPatch, ExperimentPatch, ExperimentStatus, FlashcardPatch,
    ListEvents, ListExperiments, ListFlashcards, ListNotes, ListProjects, ListReferences,
    ListTasks, NewEvent, NewExperiment, NewFlashcard, NewNote, NewProject, NewReference, NewTask,
    NotePatch, ProjectPatch, ProjectStatus, ReferencePatch, SyncDirection, SyncMetadata, TaskPatch,
    TaskPriority, TaskStatus,
};

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory()
        .await
        .expect("open in-memory data store")
}

#[tokio::test]
async fn opens_and_runs_migrations() {
    let store = fresh_store().await;
    let projects = store.list_projects(ListProjects::default()).await.unwrap();
    assert!(projects.is_empty());
    let notes = store.list_notes(ListNotes::default()).await.unwrap();
    assert!(notes.is_empty());
}

#[tokio::test]
async fn opens_on_disk_and_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/levshell.db");
    let store = DataStore::open(&path).await.expect("open on disk");
    assert!(path.exists(), "db file should exist after open");
    drop(store);

    // Re-open and confirm migrations don't re-run / break.
    let store = DataStore::open(&path).await.expect("re-open on disk");
    let projects = store.list_projects(ListProjects::default()).await.unwrap();
    assert!(projects.is_empty());
}

#[tokio::test]
async fn project_round_trip() {
    let store = fresh_store().await;

    let inserted = store
        .insert_project(NewProject {
            name: "Levshell".into(),
            status: ProjectStatus::Active,
            description: "Phase 0 scaffold".into(),
            open_questions: vec!["When does the bar render?".into()],
        })
        .await
        .unwrap();
    assert_eq!(inserted.name, "Levshell");
    assert_eq!(inserted.status, ProjectStatus::Active);
    assert_eq!(inserted.open_questions, vec!["When does the bar render?".to_string()]);

    let fetched = store.get_project(inserted.id).await.unwrap().unwrap();
    assert_eq!(fetched, inserted);

    let updated = store
        .update_project(
            inserted.id,
            ProjectPatch {
                status: Some(ProjectStatus::WritingUp),
                description: Some("Updated".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.status, ProjectStatus::WritingUp);
    assert_eq!(updated.description, "Updated");
    assert_eq!(updated.name, "Levshell", "name preserved by COALESCE");
    assert!(updated.updated_at >= inserted.updated_at);

    let listed = store
        .list_projects(ListProjects {
            status: Some(ProjectStatus::WritingUp),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, inserted.id);

    let deleted = store.delete_project(inserted.id).await.unwrap();
    assert!(deleted);
    assert!(store.get_project(inserted.id).await.unwrap().is_none());
    assert!(!store.delete_project(inserted.id).await.unwrap());
}

#[tokio::test]
async fn note_round_trip_and_fts_search() {
    let store = fresh_store().await;
    let project = store
        .insert_project(NewProject {
            name: "Algorithms".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let n1 = store
        .insert_note(NewNote {
            title: "Quicksort".into(),
            content: "Quicksort uses a divide and conquer strategy to sort.".into(),
            project_id: Some(project.id),
        })
        .await
        .unwrap();
    let _n2 = store
        .insert_note(NewNote {
            title: "Heaps".into(),
            content: "A binary heap is a complete tree with the heap property.".into(),
            project_id: Some(project.id),
        })
        .await
        .unwrap();

    // Update n1 — verifies the FTS update trigger fires.
    let n1_updated = store
        .update_note(
            n1.id,
            NotePatch {
                content: Some(
                    "Quicksort uses a divide and conquer strategy to sort an array in place.".into(),
                ),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(n1_updated.content.contains("in place"));

    // FTS hit by content
    let hits = store.search_notes("divide", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, n1.id);
    assert!(hits[0].snippet.contains("<b>") && hits[0].snippet.contains("</b>"));

    // FTS hit by title
    let hits = store.search_notes("Heaps", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Heaps");

    // List filter by project_id
    let in_project = store
        .list_notes(ListNotes {
            project_id: Some(project.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(in_project.len(), 2);

    assert!(store.delete_note(n1.id).await.unwrap());
    let hits_after = store.search_notes("divide", 10).await.unwrap();
    assert!(hits_after.is_empty(), "FTS delete trigger should remove the row");
}

#[tokio::test]
async fn reference_search_round_trip() {
    let store = fresh_store().await;

    let r = store
        .insert_reference(NewReference {
            title: "Attention Is All You Need".into(),
            authors: vec!["Vaswani et al.".into()],
            year: Some(2017),
            venue: Some("NeurIPS".into()),
            doi: None,
            citekey: "vaswani2017attention".into(),
            abstract_text: Some(
                "We propose a new simple network architecture, the Transformer, based solely on attention mechanisms.".into(),
            ),
            pdf_path: None,
            reading_progress: Some(0.0),
            annotations: vec![],
            project_id: None,
        })
        .await
        .unwrap();

    let fetched = store.get_reference(r.id).await.unwrap().unwrap();
    assert_eq!(fetched.citekey, "vaswani2017attention");
    assert_eq!(fetched.year, Some(2017));

    let hits = store.search_references("Transformer", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, r.id);
    assert_eq!(hits[0].citekey, "vaswani2017attention");
}

#[tokio::test]
async fn polymorphic_tags_across_entity_types() {
    let store = fresh_store().await;
    let project = store
        .insert_project(NewProject {
            name: "Levshell".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let note = store
        .insert_note(NewNote {
            title: "Architecture".into(),
            content: "".into(),
            project_id: Some(project.id),
        })
        .await
        .unwrap();

    store
        .add_tag(project.id, EntityType::Project, "rust")
        .await
        .unwrap();
    store
        .add_tag(project.id, EntityType::Project, "shell")
        .await
        .unwrap();
    store
        .add_tag(note.id, EntityType::Note, "rust")
        .await
        .unwrap();

    let project_tags = store.get_tags(project.id, EntityType::Project).await.unwrap();
    assert_eq!(project_tags, vec!["rust".to_string(), "shell".to_string()]);

    let rust_projects = store.find_by_tag(EntityType::Project, "rust").await.unwrap();
    assert_eq!(rust_projects, vec![project.id]);
    let rust_notes = store.find_by_tag(EntityType::Note, "rust").await.unwrap();
    assert_eq!(rust_notes, vec![note.id]);

    // Project list filtered by tag should pick up the tagged project
    let listed = store
        .list_projects(ListProjects {
            tag: Some("shell".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, project.id);

    // Idempotent add
    store
        .add_tag(project.id, EntityType::Project, "rust")
        .await
        .unwrap();
    let project_tags = store.get_tags(project.id, EntityType::Project).await.unwrap();
    assert_eq!(project_tags.len(), 2, "duplicate add must not double-insert");

    // Remove
    assert!(store
        .remove_tag(project.id, EntityType::Project, "rust")
        .await
        .unwrap());
    let project_tags = store.get_tags(project.id, EntityType::Project).await.unwrap();
    assert_eq!(project_tags, vec!["shell".to_string()]);
    assert!(!store
        .remove_tag(project.id, EntityType::Project, "rust")
        .await
        .unwrap());
}

#[tokio::test]
async fn sync_metadata_round_trip() {
    let store = fresh_store().await;
    let note = store
        .insert_note(NewNote {
            title: "Imported".into(),
            content: "From Obsidian".into(),
            project_id: None,
        })
        .await
        .unwrap();

    let meta = SyncMetadata {
        entity_id: note.id,
        entity_type: EntityType::Note,
        provider: "obsidian".into(),
        external_id: "vault/notes/imported.md".into(),
        last_synced_at: chrono::Utc::now(),
        sync_direction: SyncDirection::ImportOnly,
        sync_hash: Some("deadbeef".into()),
    };
    store.set_sync_metadata(meta.clone()).await.unwrap();

    let fetched = store
        .get_sync_metadata(note.id, EntityType::Note, "obsidian")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.provider, "obsidian");
    assert_eq!(fetched.sync_direction, SyncDirection::ImportOnly);
    assert_eq!(fetched.external_id, "vault/notes/imported.md");
    assert_eq!(fetched.sync_hash.as_deref(), Some("deadbeef"));

    // Upsert: change direction
    let mut updated = meta.clone();
    updated.sync_direction = SyncDirection::Bidirectional;
    store.set_sync_metadata(updated).await.unwrap();
    let after = store
        .get_sync_metadata(note.id, EntityType::Note, "obsidian")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.sync_direction, SyncDirection::Bidirectional);

    assert!(store
        .clear_sync_metadata(note.id, EntityType::Note, "obsidian")
        .await
        .unwrap());
    assert!(store
        .get_sync_metadata(note.id, EntityType::Note, "obsidian")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn reference_list_update_delete() {
    let store = fresh_store().await;

    let r = store
        .insert_reference(NewReference {
            title: "Deep Learning".into(),
            authors: vec!["Goodfellow".into(), "Bengio".into()],
            year: Some(2016),
            citekey: "goodfellow2016deep".into(),
            abstract_text: Some("Textbook on deep learning.".into()),
            reading_progress: Some(0.25),
            ..Default::default()
        })
        .await
        .unwrap();

    let listed = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, r.id);

    let updated = store
        .update_reference(
            r.id,
            ReferencePatch {
                reading_progress: Some(Some(0.75)),
                venue: Some(Some("MIT Press".into())),
                authors: Some(vec!["Goodfellow, Bengio, Courville".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.reading_progress, Some(0.75));
    assert_eq!(updated.venue.as_deref(), Some("MIT Press"));
    assert_eq!(updated.authors, vec!["Goodfellow, Bengio, Courville".to_string()]);
    assert_eq!(updated.title, "Deep Learning", "title unchanged by COALESCE");

    // Clearing a nullable field
    let cleared = store
        .update_reference(
            r.id,
            ReferencePatch {
                abstract_text: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.abstract_text.is_none());

    assert!(store.delete_reference(r.id).await.unwrap());
    assert!(store.get_reference(r.id).await.unwrap().is_none());
}

#[tokio::test]
async fn flashcard_round_trip_with_srs_state() {
    let store = fresh_store().await;
    let note = store
        .insert_note(NewNote {
            title: "Spanish vocab".into(),
            content: "".into(),
            project_id: None,
        })
        .await
        .unwrap();

    let due = Utc::now() + Duration::days(1);
    let card = store
        .insert_flashcard(NewFlashcard {
            front: "hola".into(),
            back: "hello".into(),
            linked_note_id: Some(note.id),
            linked_ref_id: None,
            project_id: None,
            interval_days: 1.0,
            ease_factor: 2.5,
            due_at: due,
        })
        .await
        .unwrap();
    assert_eq!(card.review_count, 0);
    assert!(card.last_reviewed.is_none());
    assert_eq!(card.linked_note_id, Some(note.id));

    // Review: schedule further out, record review.
    let new_due = Utc::now() + Duration::days(6);
    let reviewed_now = Utc::now();
    let reviewed = store
        .update_flashcard(
            card.id,
            FlashcardPatch {
                interval_days: Some(6.0),
                due_at: Some(new_due),
                review_count: Some(1),
                last_reviewed: Some(Some(reviewed_now)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reviewed.review_count, 1);
    assert!(reviewed.last_reviewed.is_some());

    // Listing due cards (due_before now + 10 days should match the 6-day one)
    let due_soon = store
        .list_flashcards(ListFlashcards {
            due_before: Some(Utc::now() + Duration::days(10)),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(due_soon.len(), 1);

    // ON DELETE SET NULL: deleting the note should clear linked_note_id
    assert!(store.delete_note(note.id).await.unwrap());
    let after = store.get_flashcard(card.id).await.unwrap().unwrap();
    assert!(after.linked_note_id.is_none(), "linked_note_id should be nulled on note deletion");

    assert!(store.delete_flashcard(card.id).await.unwrap());
}

#[tokio::test]
async fn event_round_trip_and_time_windowing() {
    let store = fresh_store().await;

    let now = Utc::now();
    let past = store
        .insert_event(NewEvent {
            title: "Yesterday".into(),
            start_at: now - Duration::days(1),
            end_at: now - Duration::days(1) + Duration::hours(1),
            location: None,
            description: None,
            url: None,
            project_id: None,
            recurrence: None,
            reminders: vec![],
        })
        .await
        .unwrap();
    let soon = store
        .insert_event(NewEvent {
            title: "Meeting".into(),
            start_at: now + Duration::hours(2),
            end_at: now + Duration::hours(3),
            location: Some("Room 101".into()),
            description: None,
            url: Some("https://meet.example/abc".into()),
            project_id: None,
            recurrence: None,
            reminders: vec!["-15m".into()],
        })
        .await
        .unwrap();

    let upcoming = store
        .list_events(ListEvents {
            after: Some(now),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(upcoming.len(), 1);
    assert_eq!(upcoming[0].id, soon.id);
    assert_eq!(upcoming[0].reminders, vec!["-15m".to_string()]);

    // Update: move the meeting later, clear URL
    let moved = store
        .update_event(
            soon.id,
            EventPatch {
                start_at: Some(now + Duration::hours(4)),
                end_at: Some(now + Duration::hours(5)),
                url: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(moved.url.is_none());

    assert!(store.delete_event(past.id).await.unwrap());
    assert!(store.delete_event(soon.id).await.unwrap());
}

#[tokio::test]
async fn task_round_trip_with_enum_and_priority() {
    let store = fresh_store().await;

    let t = store
        .insert_task(NewTask {
            title: "Write Phase 2 design".into(),
            description: Some("Sync adapters and research features".into()),
            status: TaskStatus::Pending,
            priority: Some(TaskPriority::High),
            due_at: Some(Utc::now() + Duration::days(3)),
            project_id: None,
        })
        .await
        .unwrap();
    assert_eq!(t.status, TaskStatus::Pending);
    assert_eq!(t.priority, Some(TaskPriority::High));

    let activated = store
        .update_task(
            t.id,
            TaskPatch {
                status: Some(TaskStatus::Active),
                priority: Some(Some(TaskPriority::Urgent)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(activated.status, TaskStatus::Active);
    assert_eq!(activated.priority, Some(TaskPriority::Urgent));

    // Clear priority
    let cleared = store
        .update_task(
            t.id,
            TaskPatch {
                priority: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.priority.is_none());

    let active = store
        .list_tasks(ListTasks {
            status: Some(TaskStatus::Active),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, t.id);

    assert!(store.delete_task(t.id).await.unwrap());
}

#[tokio::test]
async fn experiment_requires_project_and_round_trips() {
    let store = fresh_store().await;
    let project = store
        .insert_project(NewProject {
            name: "LLM Alignment".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let exp = store
        .insert_experiment(NewExperiment {
            name: "Baseline run".into(),
            project_id: project.id,
            hypothesis: Some("Smaller models generalize better with cleaner data".into()),
            status: ExperimentStatus::Queued,
            host: Some("gpu-cluster-3".into()),
            git_hash: Some("abc123".into()),
            config: Some(serde_json::json!({ "lr": 1e-4, "batch": 32 })),
            notes: None,
        })
        .await
        .unwrap();
    assert_eq!(exp.status, ExperimentStatus::Queued);
    assert_eq!(exp.config.as_ref().unwrap()["batch"], 32);
    assert_eq!(exp.metrics, serde_json::json!({}));

    let started_now = Utc::now();
    let running = store
        .update_experiment(
            exp.id,
            ExperimentPatch {
                status: Some(ExperimentStatus::Running),
                started_at: Some(Some(started_now)),
                metrics: Some(serde_json::json!({ "step": 100, "loss": 2.34 })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(running.status, ExperimentStatus::Running);
    assert!(running.started_at.is_some());
    assert_eq!(running.metrics["loss"], 2.34);

    let filtered = store
        .list_experiments(ListExperiments {
            project_id: Some(project.id),
            status: Some(ExperimentStatus::Running),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);

    // ON DELETE CASCADE: deleting project removes experiments
    assert!(store.delete_project(project.id).await.unwrap());
    assert!(store.get_experiment(exp.id).await.unwrap().is_none());
}
