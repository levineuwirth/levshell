//! Integration tests for `levshell_sync::ObsidianAdapter`.
//!
//! Each test builds a tempdir vault, constructs an adapter pointed at it,
//! and runs `sync()` directly (bypassing the engine's scheduling loop).
//! Verifies insert / update / delete / conflict / tag / frontmatter-title
//! behavior end-to-end through the real DataStore.

use std::fs;
use std::path::Path;
use std::time::Duration;

use levshell_data::{DataStore, EntityType, ListNotes};
use levshell_sync::obsidian::PROVIDER_NAME;
use levshell_sync::{ObsidianAdapter, ObsidianConfig, SyncAdapter, SyncContext, SyncStatus};
use tempfile::TempDir;

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.expect("open store")
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn make_adapter(vault: &TempDir) -> ObsidianAdapter {
    ObsidianAdapter::new(ObsidianConfig {
        vault_path: vault.path().to_path_buf(),
        enabled: true,
        poll_interval_secs: 60,
        exclude_dirs: vec![".obsidian".into(), ".trash".into(), ".git".into()],
    })
}

fn ctx(store: &DataStore) -> SyncContext {
    SyncContext {
        store: store.clone(),
        since: None,
    }
}

#[tokio::test]
async fn probe_is_unavailable_when_vault_missing() {
    let store = fresh_store().await;
    let adapter = ObsidianAdapter::new(ObsidianConfig {
        vault_path: "/this/path/does/not/exist".into(),
        enabled: true,
        poll_interval_secs: 60,
        exclude_dirs: vec![],
    });
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_is_unavailable_when_disabled() {
    let store = fresh_store().await;
    let vault = TempDir::new().unwrap();
    let adapter = ObsidianAdapter::new(ObsidianConfig {
        vault_path: vault.path().to_path_buf(),
        enabled: false,
        poll_interval_secs: 60,
        exclude_dirs: vec![],
    });
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn poll_interval_respects_config() {
    let vault = TempDir::new().unwrap();
    let adapter = ObsidianAdapter::new(ObsidianConfig {
        vault_path: vault.path().to_path_buf(),
        enabled: true,
        poll_interval_secs: 42,
        exclude_dirs: vec![],
    });
    assert_eq!(adapter.poll_interval(), Duration::from_secs(42));
}

#[tokio::test]
async fn initial_sync_inserts_notes_and_tags() {
    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "hello.md", "# Hello\n\nWorld.\n");
    write_file(
        vault.path(),
        "projects/levshell.md",
        "---\ntitle: Levshell Design\ntags: rust, shell\n---\n# Notes\n",
    );
    write_file(vault.path(), ".obsidian/config.json", "ignored");
    write_file(vault.path(), "subdir/not_markdown.txt", "ignored");

    let store = fresh_store().await;
    let adapter = make_adapter(&vault);
    let report = adapter.sync(&ctx(&store)).await.unwrap();

    assert_eq!(report.upserted, 2, "one insert per .md file");
    assert_eq!(report.deleted, 0);
    assert!(report.conflicts.is_empty());

    let notes = store.list_notes(ListNotes::default()).await.unwrap();
    assert_eq!(notes.len(), 2);

    // Find the frontmatter-titled note — expect the YAML title, not
    // the filename.
    let design = notes.iter().find(|n| n.title == "Levshell Design").unwrap();
    assert!(design.content.contains("# Notes"));
    assert!(!design.content.contains("---"), "frontmatter stripped from body");

    let design_tags = store
        .get_tags(design.id, EntityType::Note)
        .await
        .unwrap();
    assert_eq!(design_tags, vec!["rust".to_string(), "shell".to_string()]);

    let hello = notes.iter().find(|n| n.title == "hello").unwrap();
    let hello_tags = store.get_tags(hello.id, EntityType::Note).await.unwrap();
    assert!(hello_tags.is_empty(), "no tags when no frontmatter");

    // sync_metadata covers every Note
    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    assert_eq!(metas.len(), 2);
    let external_ids: std::collections::HashSet<_> =
        metas.iter().map(|m| m.external_id.as_str()).collect();
    assert!(external_ids.contains("hello.md"));
    assert!(external_ids.contains("projects/levshell.md"));
}

#[tokio::test]
async fn unchanged_file_is_not_re_upserted() {
    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "a.md", "stable content\n");
    let store = fresh_store().await;
    let adapter = make_adapter(&vault);

    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 1);

    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(second.upserted, 0, "hash match → skip");
    assert_eq!(second.deleted, 0);
    assert!(second.conflicts.is_empty());
}

#[tokio::test]
async fn edited_file_updates_note_and_tags() {
    let vault = TempDir::new().unwrap();
    write_file(
        vault.path(),
        "note.md",
        "---\ntags: [old]\n---\noriginal body\n",
    );
    let store = fresh_store().await;
    let adapter = make_adapter(&vault);

    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 1);

    // Rewrite with new tags and content.
    write_file(
        vault.path(),
        "note.md",
        "---\ntags: [new, shiny]\n---\nrevised body\n",
    );

    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(second.upserted, 1);

    let notes = store.list_notes(ListNotes::default()).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].content.contains("revised body"));

    let tags = store.get_tags(notes[0].id, EntityType::Note).await.unwrap();
    assert_eq!(tags, vec!["new".to_string(), "shiny".to_string()]);
}

#[tokio::test]
async fn removed_file_deletes_note() {
    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "keep.md", "a");
    write_file(vault.path(), "remove.md", "b");
    let store = fresh_store().await;
    let adapter = make_adapter(&vault);

    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 2);

    fs::remove_file(vault.path().join("remove.md")).unwrap();

    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(second.deleted, 1);
    assert_eq!(second.upserted, 0);

    let notes = store.list_notes(ListNotes::default()).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].title, "keep");

    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    assert_eq!(metas.len(), 1);
}

#[tokio::test]
async fn local_edit_before_external_change_surfaces_conflict() {
    use levshell_data::NotePatch;

    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "n.md", "v1");
    let store = fresh_store().await;
    let adapter = make_adapter(&vault);

    adapter.sync(&ctx(&store)).await.unwrap();

    let note = store.list_notes(ListNotes::default()).await.unwrap()[0].clone();

    // Local edit after the last sync. SQLite TEXT timestamps have 1-second
    // granularity; sleep briefly to guarantee `updated_at > last_synced_at`
    // on comparison.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    store
        .update_note(
            note.id,
            NotePatch {
                content: Some("locally edited".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // External edit after the local edit.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    write_file(vault.path(), "n.md", "v2");

    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);
    assert_eq!(report.conflicts.len(), 1, "divergent edit must emit conflict");
    assert_eq!(report.conflicts[0].entity_type, EntityType::Note);
    assert_eq!(report.conflicts[0].external_id, "n.md");

    // V1 resolution: external wins.
    let after = store.get_note(note.id).await.unwrap().unwrap();
    assert_eq!(after.content, "v2");
}

#[tokio::test]
async fn nested_directories_produce_forward_slash_external_ids() {
    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "a/b/c/deep.md", "deep");
    let store = fresh_store().await;
    let adapter = make_adapter(&vault);

    adapter.sync(&ctx(&store)).await.unwrap();

    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].external_id, "a/b/c/deep.md");
}

#[tokio::test]
async fn reload_config_disables_adapter_live() {
    let vault = TempDir::new().unwrap();
    write_file(vault.path(), "a.md", "hello");

    let adapter = make_adapter(&vault);
    let store = fresh_store().await;

    // Initially enabled → probe healthy, first sync inserts.
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Healthy);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);

    // Reload with enabled=false → probe flips to Unavailable and sync
    // short-circuits to an empty report.
    let mut disabled = adapter.current_config();
    disabled.enabled = false;
    adapter.reload_config(disabled);
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert!(report.is_empty(), "disabled adapter must not write anything");
}

#[tokio::test]
async fn reload_config_switches_vault_path_live() {
    let vault_a = TempDir::new().unwrap();
    write_file(vault_a.path(), "in_a.md", "a");
    let vault_b = TempDir::new().unwrap();
    write_file(vault_b.path(), "in_b.md", "b");

    let adapter = make_adapter(&vault_a);
    let store = fresh_store().await;

    // Sync the first vault.
    adapter.sync(&ctx(&store)).await.unwrap();
    let titles = store
        .list_notes(ListNotes::default())
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.title)
        .collect::<std::collections::HashSet<_>>();
    assert!(titles.contains("in_a"));

    // Hot-reload onto vault B. Next sync:
    //   - notes from vault A no longer appear on disk → they're deleted
    //   - in_b.md appears → inserted
    let mut switched = adapter.current_config();
    switched.vault_path = vault_b.path().to_path_buf();
    adapter.reload_config(switched);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1, "vault B's in_b.md should be inserted");
    assert_eq!(report.deleted, 1, "vault A's in_a.md should be removed");

    let titles = store
        .list_notes(ListNotes::default())
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.title)
        .collect::<std::collections::HashSet<_>>();
    assert!(titles.contains("in_b"));
    assert!(!titles.contains("in_a"), "vault A's note should have been removed");
}
