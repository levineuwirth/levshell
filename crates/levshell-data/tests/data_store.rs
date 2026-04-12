//! Integration tests for `levshell-data::DataStore`.
//!
//! Each test opens a fresh in-memory or tempfile-backed database, runs the
//! embedded migration, and exercises one slice of the public API. Together
//! they cover the Phase 0 acceptance criteria from spec §6.1 step 0.2:
//! migrations apply, Project + Note round-trip, FTS search returns hits, the
//! polymorphic tag table works across entity types, and the sync_metadata
//! provenance API stores and reads back.

use levshell_data::{
    DataStore, EntityType, ListNotes, ListProjects, NewNote, NewProject, NewReference, NotePatch,
    ProjectPatch, ProjectStatus, SyncDirection, SyncMetadata,
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
