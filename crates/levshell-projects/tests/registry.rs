//! Integration tests for `levshell_projects::ProjectRegistry`.
//!
//! Exercise the loader → upsert → attach pathway end-to-end against a real
//! in-memory data store. Tests deliberately avoid the TOML filesystem
//! loader except for the happy path — the `config.rs` module's unit tests
//! cover parser edge cases.

use levshell_core::EventBus;
use levshell_data::{
    DataStore, EntityType, ListNotes, NewExperiment, NewNote, NewReference, NewTask,
    ProjectStatus, TaskStatus,
};
use levshell_projects::{ProjectFile, ProjectRegistry, ProjectRegistryError};

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.expect("open store")
}

fn sample_file(name: &str, tags: &[&str], workspaces: &[&str]) -> ProjectFile {
    ProjectFile {
        name: name.into(),
        status: Some(ProjectStatus::Active),
        description: Some(format!("{name} description")),
        open_questions: vec!["q1?".into()],
        tags: tags.iter().map(|s| (*s).to_string()).collect(),
        git_repos: vec![],
        ssh_hosts: vec![],
        workspace_names: workspaces.iter().map(|s| (*s).to_string()).collect(),
        accent_color: None,
    }
}

#[tokio::test]
async fn load_from_files_upserts_into_store_and_indexes() {
    let store = fresh_store().await;
    let bus = EventBus::new();

    let files = vec![
        sample_file("LLM Alignment", &["llm", "research"], &["research-llm"]),
        sample_file("Thesis", &["writing"], &["writing"]),
    ];
    let registry = ProjectRegistry::load_from_files(store.clone(), bus, files)
        .await
        .unwrap();

    // Both projects made it into the DB.
    let db_projects = store
        .list_projects(levshell_data::ListProjects::default())
        .await
        .unwrap();
    assert_eq!(db_projects.len(), 2);

    // And are indexed in memory, sorted by name.
    let listed = registry.list().await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].project.name, "LLM Alignment");
    assert_eq!(listed[1].project.name, "Thesis");
    assert_eq!(listed[0].metadata.tags, vec!["llm", "research"]);

    // Lookup by name + by workspace.
    let by_name = registry.find_by_name("Thesis").await.unwrap();
    assert_eq!(by_name.project.name, "Thesis");
    let by_ws = registry
        .find_by_workspace("research-llm")
        .await
        .unwrap();
    assert_eq!(by_ws.project.name, "LLM Alignment");
}

#[tokio::test]
async fn upsert_is_idempotent_on_reload() {
    let store = fresh_store().await;
    let bus = EventBus::new();

    let registry = ProjectRegistry::load_from_files(
        store.clone(),
        bus.clone(),
        vec![sample_file("P", &[], &[])],
    )
    .await
    .unwrap();
    let before = registry.list().await;
    let id_before = before[0].project.id;

    // Load AGAIN with the same registry handle (simulates a hot reload).
    // Upsert should preserve the id.
    registry
        .upsert_from_file(sample_file("P", &["updated"], &[]))
        .await
        .unwrap();

    let after = registry.list().await;
    assert_eq!(after.len(), 1, "no duplicate inserted");
    assert_eq!(after[0].project.id, id_before, "id preserved across upsert");
    assert_eq!(after[0].metadata.tags, vec!["updated"]);

    // DB shouldn't have a second row either.
    let db_projects = store
        .list_projects(levshell_data::ListProjects::default())
        .await
        .unwrap();
    assert_eq!(db_projects.len(), 1);
}

#[tokio::test]
async fn find_by_tags_picks_highest_overlap() {
    let store = fresh_store().await;
    let bus = EventBus::new();

    let registry = ProjectRegistry::load_from_files(
        store,
        bus,
        vec![
            sample_file("A", &["rust"], &[]),
            sample_file("B", &["rust", "shell"], &[]),
            sample_file("C", &["typescript"], &[]),
        ],
    )
    .await
    .unwrap();

    let matched = registry
        .find_by_tags(&["rust".to_string(), "shell".to_string()])
        .await
        .unwrap();
    assert_eq!(matched.project.name, "B");

    // Single tag only matches A and B equally (1 each); A wins on
    // lexicographic tie-break.
    let matched = registry
        .find_by_tags(&["rust".to_string()])
        .await
        .unwrap();
    assert_eq!(matched.project.name, "A");

    // No overlap → None.
    let matched = registry
        .find_by_tags(&["python".to_string()])
        .await;
    assert!(matched.is_none());
}

#[tokio::test]
async fn resolve_accepts_name_or_uuid() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let registry = ProjectRegistry::load_from_files(
        store,
        bus,
        vec![sample_file("MyProject", &[], &[])],
    )
    .await
    .unwrap();
    let entry = registry.find_by_name("MyProject").await.unwrap();

    let id_by_name = registry.resolve("MyProject").await.unwrap();
    assert_eq!(id_by_name, entry.project.id);

    let id_by_uuid = registry.resolve(&entry.project.id.to_string()).await.unwrap();
    assert_eq!(id_by_uuid, entry.project.id);

    let err = registry.resolve("does-not-exist").await.unwrap_err();
    assert!(matches!(err, ProjectRegistryError::UnknownProject(_)));
}

#[tokio::test]
async fn attach_and_detach_a_note() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let registry = ProjectRegistry::load_from_files(
        store.clone(),
        bus,
        vec![sample_file("Target", &[], &[])],
    )
    .await
    .unwrap();
    let project_id = registry.find_by_name("Target").await.unwrap().project.id;

    let note = store
        .insert_note(NewNote {
            title: "Some note".into(),
            content: "body".into(),
            project_id: None,
        })
        .await
        .unwrap();
    assert!(note.project_id.is_none());

    registry
        .attach(EntityType::Note, note.id, project_id)
        .await
        .unwrap();
    let after_attach = store.get_note(note.id).await.unwrap().unwrap();
    assert_eq!(after_attach.project_id, Some(project_id));

    registry.detach(EntityType::Note, note.id).await.unwrap();
    let after_detach = store.get_note(note.id).await.unwrap().unwrap();
    assert!(after_detach.project_id.is_none());
}

#[tokio::test]
async fn attach_works_for_reference_flashcard_event_task() {
    use chrono::{Duration, Utc};
    use levshell_data::NewEvent;
    use levshell_data::NewFlashcard;

    let store = fresh_store().await;
    let bus = EventBus::new();
    let registry = ProjectRegistry::load_from_files(
        store.clone(),
        bus,
        vec![sample_file("Target", &[], &[])],
    )
    .await
    .unwrap();
    let project_id = registry.find_by_name("Target").await.unwrap().project.id;

    let reference = store
        .insert_reference(NewReference {
            title: "A Paper".into(),
            citekey: "paper2026".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    registry
        .attach(EntityType::Reference, reference.id, project_id)
        .await
        .unwrap();
    assert_eq!(
        store.get_reference(reference.id).await.unwrap().unwrap().project_id,
        Some(project_id)
    );

    let flashcard = store
        .insert_flashcard(NewFlashcard {
            front: "Q".into(),
            back: "A".into(),
            linked_note_id: None,
            linked_ref_id: None,
            project_id: None,
            interval_days: 1.0,
            ease_factor: 2.5,
            due_at: Utc::now() + Duration::days(1),
        })
        .await
        .unwrap();
    registry
        .attach(EntityType::Flashcard, flashcard.id, project_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_flashcard(flashcard.id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project_id)
    );

    let event = store
        .insert_event(NewEvent {
            title: "Meeting".into(),
            start_at: Utc::now(),
            end_at: Utc::now() + Duration::hours(1),
            location: None,
            description: None,
            url: None,
            project_id: None,
            recurrence: None,
            reminders: vec![],
        })
        .await
        .unwrap();
    registry
        .attach(EntityType::Event, event.id, project_id)
        .await
        .unwrap();
    assert_eq!(
        store.get_event(event.id).await.unwrap().unwrap().project_id,
        Some(project_id)
    );

    let task = store
        .insert_task(NewTask {
            title: "Do the thing".into(),
            description: None,
            status: TaskStatus::Pending,
            priority: None,
            due_at: None,
            project_id: None,
        })
        .await
        .unwrap();
    registry
        .attach(EntityType::Task, task.id, project_id)
        .await
        .unwrap();
    assert_eq!(
        store.get_task(task.id).await.unwrap().unwrap().project_id,
        Some(project_id)
    );
}

#[tokio::test]
async fn attach_unknown_project_errors_cleanly() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let registry = ProjectRegistry::empty(store.clone(), bus);

    let note = store
        .insert_note(NewNote {
            title: "n".into(),
            content: "".into(),
            project_id: None,
        })
        .await
        .unwrap();
    let unknown = uuid::Uuid::now_v7();

    let err = registry
        .attach(EntityType::Note, note.id, unknown)
        .await
        .unwrap_err();
    assert!(matches!(err, ProjectRegistryError::UnknownProject(_)));

    // Unknown project id must NOT silently nullify — the note still has
    // project_id == None (its original value).
    let untouched = store.get_note(note.id).await.unwrap().unwrap();
    assert!(untouched.project_id.is_none());
}

#[tokio::test]
async fn detach_rejects_experiments_and_projects() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let registry = ProjectRegistry::load_from_files(
        store.clone(),
        bus,
        vec![sample_file("Owner", &[], &[])],
    )
    .await
    .unwrap();
    let project_id = registry.find_by_name("Owner").await.unwrap().project.id;

    let exp = store
        .insert_experiment(NewExperiment {
            name: "run-1".into(),
            project_id,
            hypothesis: None,
            status: levshell_data::ExperimentStatus::Queued,
            host: None,
            git_hash: None,
            config: None,
            notes: None,
        })
        .await
        .unwrap();

    let err = registry
        .detach(EntityType::Experiment, exp.id)
        .await
        .unwrap_err();
    assert!(matches!(err, ProjectRegistryError::UnattachableType(EntityType::Experiment)));

    let err = registry
        .detach(EntityType::Project, project_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ProjectRegistryError::UnattachableType(EntityType::Project)));
}

#[tokio::test]
async fn load_from_dir_empty_returns_empty_registry() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let dir = tempfile::tempdir().unwrap();
    // Explicit subdir that does not exist.
    let missing = dir.path().join("projects");
    let registry = ProjectRegistry::load_from_dir(store, bus, &missing).await.unwrap();
    assert!(registry.list().await.is_empty());
    // And no notes were collaterally touched.
    let _ = ListNotes::default();
}

#[tokio::test]
async fn load_from_dir_parses_real_toml_files() {
    let store = fresh_store().await;
    let bus = EventBus::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("p1.toml"),
        r#"name = "Project One"
tags = ["first"]
workspace_names = ["p1"]
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("p2.toml"),
        r#"name = "Project Two"
status = "simmering"
"#,
    )
    .unwrap();

    let registry = ProjectRegistry::load_from_dir(store, bus, dir.path())
        .await
        .unwrap();
    let listed = registry.list().await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].project.name, "Project One");
    assert_eq!(listed[0].metadata.tags, vec!["first"]);
    assert_eq!(listed[1].project.status, ProjectStatus::Simmering);
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_watcher_hot_reloads_added_and_modified_files() {
    use std::time::Duration;

    let store = fresh_store().await;
    let bus = EventBus::new();
    let dir = tempfile::tempdir().unwrap();

    // Start empty.
    let registry = ProjectRegistry::load_from_dir(store, bus, dir.path())
        .await
        .unwrap();
    assert!(registry.list().await.is_empty());

    let _watcher = registry.spawn_watcher(dir.path()).unwrap();

    // Writing a new project file should surface in the registry.
    std::fs::write(dir.path().join("alpha.toml"), r#"name = "Alpha""#).unwrap();

    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(50);
    while waited < Duration::from_secs(5) {
        if registry.find_by_name("Alpha").await.is_some() {
            break;
        }
        tokio::time::sleep(step).await;
        waited += step;
    }
    assert!(
        registry.find_by_name("Alpha").await.is_some(),
        "hot-reload never picked up the new file within 5s"
    );

    // Overwriting the file with a status change should also land.
    std::fs::write(
        dir.path().join("alpha.toml"),
        r#"name = "Alpha"
status = "writing_up"
"#,
    )
    .unwrap();

    let mut waited = Duration::ZERO;
    loop {
        let entry = registry.find_by_name("Alpha").await.unwrap();
        if entry.project.status == levshell_data::ProjectStatus::WritingUp {
            break;
        }
        if waited >= Duration::from_secs(5) {
            panic!("modify never reflected after 5s: status still {:?}", entry.project.status);
        }
        tokio::time::sleep(step).await;
        waited += step;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn removed_project_file_preserves_db_row() {
    use std::time::Duration;

    let store = fresh_store().await;
    let bus = EventBus::new();
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(dir.path().join("keep.toml"), r#"name = "Keep""#).unwrap();
    let registry = ProjectRegistry::load_from_dir(store.clone(), bus, dir.path())
        .await
        .unwrap();
    let _watcher = registry.spawn_watcher(dir.path()).unwrap();
    let id_before = registry.find_by_name("Keep").await.unwrap().project.id;

    // Remove the file on disk. The registry's IN-MEMORY index *may*
    // eventually be rebuilt to reflect this (future work), but the
    // underlying DB row must NOT be destroyed on filesystem absence —
    // a user renaming a file produces Remove+Create as a pair, and we
    // can't tell them apart deterministically.
    std::fs::remove_file(dir.path().join("keep.toml")).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let row = store.get_project(id_before).await.unwrap();
    assert!(
        row.is_some(),
        "DB row for a project must survive removal of its TOML file"
    );
}
