//! Integration tests for `levshell_sync::ZoteroAdapter`.
//!
//! Each test seeds a tempfile Zotero-shaped SQLite database, points the
//! adapter at it, and runs `sync()` directly. Verifies insert / update /
//! delete / tag / citekey / conflict behavior against the real
//! `DataStore`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use levshell_data::{DataStore, EntityType, ListReferences};
use levshell_sync::zotero::PROVIDER_NAME;
use levshell_sync::{SyncAdapter, SyncContext, SyncStatus, ZoteroAdapter, ZoteroConfig};
use rusqlite::{params, Connection};
use tempfile::TempDir;

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.expect("open store")
}

fn ctx(store: &DataStore) -> SyncContext {
    SyncContext {
        store: store.clone(),
        since: None,
    }
}

fn make_adapter(db: &Path) -> ZoteroAdapter {
    ZoteroAdapter::new(ZoteroConfig {
        database_path: db.to_path_buf(),
        enabled: true,
        poll_interval_secs: 60,
        libraries: Vec::new(),
    })
}

/// Minimal Zotero schema — just the tables our adapter reads. Real
/// zotero.sqlite has ~80 tables; none of the others are relevant.
fn init_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE items (
            itemID INTEGER PRIMARY KEY,
            itemTypeID INTEGER NOT NULL,
            dateModified TEXT NOT NULL,
            libraryID INTEGER NOT NULL,
            key TEXT NOT NULL UNIQUE
        );
        CREATE TABLE itemTypes (
            itemTypeID INTEGER PRIMARY KEY,
            typeName TEXT NOT NULL
        );
        CREATE TABLE fields (
            fieldID INTEGER PRIMARY KEY,
            fieldName TEXT NOT NULL
        );
        CREATE TABLE itemData (
            itemID INTEGER,
            fieldID INTEGER,
            valueID INTEGER,
            PRIMARY KEY (itemID, fieldID)
        );
        CREATE TABLE itemDataValues (
            valueID INTEGER PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE creators (
            creatorID INTEGER PRIMARY KEY,
            firstName TEXT,
            lastName TEXT,
            fieldMode INTEGER
        );
        CREATE TABLE itemCreators (
            itemID INTEGER,
            creatorID INTEGER,
            creatorTypeID INTEGER,
            orderIndex INTEGER,
            PRIMARY KEY (itemID, orderIndex)
        );
        CREATE TABLE creatorTypes (
            creatorTypeID INTEGER PRIMARY KEY,
            creatorType TEXT NOT NULL
        );
        CREATE TABLE tags (
            tagID INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE itemTags (
            itemID INTEGER,
            tagID INTEGER,
            PRIMARY KEY (itemID, tagID)
        );
        CREATE TABLE deletedItems (
            itemID INTEGER PRIMARY KEY
        );
        CREATE TABLE itemAttachments (
            itemID INTEGER PRIMARY KEY,
            parentItemID INTEGER,
            contentType TEXT,
            path TEXT
        );",
    )
    .unwrap();

    // Canonical type IDs used across tests.
    conn.execute(
        "INSERT INTO itemTypes (itemTypeID, typeName) VALUES
            (1, 'journalArticle'), (2, 'book'), (3, 'preprint'),
            (4, 'attachment'), (5, 'note')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO creatorTypes (creatorTypeID, creatorType) VALUES
            (1, 'author'), (2, 'editor')",
        [],
    )
    .unwrap();
    // Fields we use. The actual Zotero DB has ~140 fields; we only
    // need the ones the adapter reads.
    conn.execute(
        "INSERT INTO fields (fieldID, fieldName) VALUES
            (1, 'title'), (2, 'date'), (3, 'DOI'), (4, 'url'),
            (5, 'publicationTitle'), (6, 'abstractNote'), (7, 'extra'),
            (8, 'conferenceName'), (9, 'bookTitle')",
        [],
    )
    .unwrap();
}

/// Add one journal-article item with the given fields and creators.
/// Returns the assigned itemID so the caller can set tags / mark deleted.
#[allow(clippy::too_many_arguments)]
fn insert_item(
    conn: &Connection,
    item_type_id: i64,
    library_id: i64,
    key: &str,
    date_modified: &str,
    title: Option<&str>,
    date: Option<&str>,
    doi: Option<&str>,
    publication_title: Option<&str>,
    abstract_note: Option<&str>,
    extra: Option<&str>,
    creators: &[(&str, &str)], // (first, last) as authors
) -> i64 {
    conn.execute(
        "INSERT INTO items (itemTypeID, dateModified, libraryID, key) VALUES (?1, ?2, ?3, ?4)",
        params![item_type_id, date_modified, library_id, key],
    )
    .unwrap();
    let item_id = conn.last_insert_rowid();

    let set_field = |field_id: i64, value: &str| {
        conn.execute(
            "INSERT INTO itemDataValues (value) VALUES (?1)",
            params![value],
        )
        .unwrap();
        let value_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO itemData (itemID, fieldID, valueID) VALUES (?1, ?2, ?3)",
            params![item_id, field_id, value_id],
        )
        .unwrap();
    };

    if let Some(v) = title {
        set_field(1, v);
    }
    if let Some(v) = date {
        set_field(2, v);
    }
    if let Some(v) = doi {
        set_field(3, v);
    }
    if let Some(v) = publication_title {
        set_field(5, v);
    }
    if let Some(v) = abstract_note {
        set_field(6, v);
    }
    if let Some(v) = extra {
        set_field(7, v);
    }

    for (idx, (first, last)) in creators.iter().enumerate() {
        conn.execute(
            "INSERT INTO creators (firstName, lastName, fieldMode) VALUES (?1, ?2, 0)",
            params![first, last],
        )
        .unwrap();
        let creator_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO itemCreators (itemID, creatorID, creatorTypeID, orderIndex) \
             VALUES (?1, ?2, 1, ?3)",
            params![item_id, creator_id, idx as i64],
        )
        .unwrap();
    }

    item_id
}

fn add_tag(conn: &Connection, item_id: i64, name: &str) {
    conn.execute(
        "INSERT INTO tags (name) VALUES (?1) ON CONFLICT DO NOTHING",
        params![name],
    )
    .unwrap();
    let tag_id: i64 = conn
        .query_row("SELECT tagID FROM tags WHERE name = ?1", params![name], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO itemTags (itemID, tagID) VALUES (?1, ?2)",
        params![item_id, tag_id],
    )
    .unwrap();
}

fn mark_deleted(conn: &Connection, item_id: i64) {
    conn.execute(
        "INSERT INTO deletedItems (itemID) VALUES (?1)",
        params![item_id],
    )
    .unwrap();
}

fn seed_default_library(dir: &TempDir) -> PathBuf {
    let path = dir.path().join("zotero.sqlite");
    let conn = Connection::open(&path).unwrap();
    init_schema(&conn);
    path
}

// -----------------------------------------------------------------------

#[tokio::test]
async fn probe_is_unavailable_when_db_missing() {
    let store = fresh_store().await;
    let adapter = ZoteroAdapter::new(ZoteroConfig {
        database_path: "/this/path/does/not/exist.sqlite".into(),
        enabled: true,
        poll_interval_secs: 60,
        libraries: Vec::new(),
    });
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_is_unavailable_when_disabled() {
    let store = fresh_store().await;
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    let adapter = ZoteroAdapter::new(ZoteroConfig {
        database_path: path,
        enabled: false,
        poll_interval_secs: 60,
        libraries: Vec::new(),
    });
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn poll_interval_respects_config() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    let adapter = ZoteroAdapter::new(ZoteroConfig {
        database_path: path,
        enabled: true,
        poll_interval_secs: 42,
        libraries: Vec::new(),
    });
    assert_eq!(adapter.poll_interval(), Duration::from_secs(42));
}

#[tokio::test]
async fn initial_sync_inserts_references_and_tags() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        let a = insert_item(
            &conn,
            1, // journalArticle
            1,
            "ABC12345",
            "2024-01-01 12:00:00",
            Some("On Bar Widgets"),
            Some("2024-04-10"),
            Some("10.1/bar"),
            Some("Journal of Shells"),
            Some("Abstract body."),
            None,
            &[("Alice", "Author"), ("Bob", "Writer")],
        );
        add_tag(&conn, a, "shell");
        add_tag(&conn, a, "wm");

        insert_item(
            &conn,
            2, // book
            1,
            "BOOK0001",
            "2023-06-01 00:00:00",
            Some("Desktop History"),
            Some("2023"),
            None,
            None,
            None,
            None,
            &[("Carol", "Curator")],
        );

        // An attachment and a note — both should be skipped.
        insert_item(
            &conn,
            4,
            1,
            "ATTCH001",
            "2024-01-01 12:00:00",
            None,
            None,
            None,
            None,
            None,
            None,
            &[],
        );
        insert_item(
            &conn,
            5,
            1,
            "NOTEXXXX",
            "2024-01-01 12:00:00",
            None,
            None,
            None,
            None,
            None,
            None,
            &[],
        );
    }

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    let report = adapter.sync(&ctx(&store)).await.unwrap();

    assert_eq!(report.upserted, 2, "attachments + notes are excluded");
    assert_eq!(report.deleted, 0);

    let refs = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    assert_eq!(refs.len(), 2);

    let bar = refs.iter().find(|r| r.title == "On Bar Widgets").unwrap();
    assert_eq!(bar.authors, vec!["Alice Author", "Bob Writer"]);
    assert_eq!(bar.year, Some(2024));
    assert_eq!(bar.venue.as_deref(), Some("Journal of Shells"));
    assert_eq!(bar.doi.as_deref(), Some("10.1/bar"));
    assert_eq!(bar.abstract_text.as_deref(), Some("Abstract body."));
    assert_eq!(bar.citekey, "ABC12345");

    let tags = store.get_tags(bar.id, EntityType::Reference).await.unwrap();
    let mut sorted = tags.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["shell".to_string(), "wm".to_string()]);

    // Sync metadata is keyed on the Zotero item key.
    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    assert_eq!(metas.len(), 2);
    assert!(metas.iter().any(|m| m.external_id == "ABC12345"));
}

#[tokio::test]
async fn citekey_prefers_bbt_extra_field() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "ABC12345",
            "2024-01-01 00:00:00",
            Some("Paper"),
            Some("2024"),
            None,
            None,
            None,
            Some("tex.keywords: rust\nCitation Key: alice2024bar\nfoo: bar"),
            &[("Alice", "Author")],
        );
    }

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    adapter.sync(&ctx(&store)).await.unwrap();

    let refs = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    assert_eq!(refs[0].citekey, "alice2024bar");
}

#[tokio::test]
async fn repeat_sync_is_idempotent_when_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "ABC12345",
            "2024-01-01 00:00:00",
            Some("Paper"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("Alice", "Author")],
        );
    }

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 1);

    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(
        second.upserted, 0,
        "unchanged dateModified → skipped on re-sync"
    );
    assert_eq!(second.deleted, 0);
}

#[tokio::test]
async fn changed_date_modified_triggers_update() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "ABC12345",
            "2024-01-01 00:00:00",
            Some("Original Title"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("Alice", "Author")],
        );
    }

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    adapter.sync(&ctx(&store)).await.unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE itemDataValues SET value = 'Updated Title' \
             WHERE value = 'Original Title'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE items SET dateModified = '2024-02-01 00:00:00' WHERE key = 'ABC12345'",
            [],
        )
        .unwrap();
    }

    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);

    let refs = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].title, "Updated Title");
}

#[tokio::test]
async fn trashed_item_is_deleted_on_next_sync() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    let item_id = {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "ABC12345",
            "2024-01-01 00:00:00",
            Some("Paper"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("Alice", "Author")],
        )
    };

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(
        store
            .list_references(ListReferences::default())
            .await
            .unwrap()
            .len(),
        1
    );

    {
        let conn = Connection::open(&path).unwrap();
        mark_deleted(&conn, item_id);
    }

    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(report.upserted, 0);
    assert!(
        store
            .list_references(ListReferences::default())
            .await
            .unwrap()
            .is_empty(),
        "trashed item's reference should be gone"
    );
    assert!(
        store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await
            .unwrap()
            .is_empty(),
        "sync metadata cleaned up alongside the reference"
    );
}

#[tokio::test]
async fn library_filter_excludes_other_libraries() {
    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "USERKEY1",
            "2024-01-01 00:00:00",
            Some("User Paper"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("U", "ser")],
        );
        insert_item(
            &conn,
            1,
            9, // group library
            "GRPKEY01",
            "2024-01-01 00:00:00",
            Some("Group Paper"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("G", "roup")],
        );
    }

    let store = fresh_store().await;
    let adapter = ZoteroAdapter::new(ZoteroConfig {
        database_path: path.clone(),
        enabled: true,
        poll_interval_secs: 60,
        libraries: vec![1], // user library only
    });
    adapter.sync(&ctx(&store)).await.unwrap();

    let refs = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    assert_eq!(refs.len(), 1, "only library 1 synced");
    assert_eq!(refs[0].title, "User Paper");
}

#[tokio::test]
async fn local_edit_after_sync_flags_conflict() {
    use levshell_data::ReferencePatch;

    let dir = TempDir::new().unwrap();
    let path = seed_default_library(&dir);
    {
        let conn = Connection::open(&path).unwrap();
        insert_item(
            &conn,
            1,
            1,
            "ABC12345",
            "2024-01-01 00:00:00",
            Some("V1"),
            Some("2024"),
            None,
            None,
            None,
            None,
            &[("Alice", "Author")],
        );
    }

    let store = fresh_store().await;
    let adapter = make_adapter(&path);
    adapter.sync(&ctx(&store)).await.unwrap();

    // Simulate a user editing the reference locally *after* the sync.
    let refs = store
        .list_references(ListReferences::default())
        .await
        .unwrap();
    let local = &refs[0];
    store
        .update_reference(
            local.id,
            ReferencePatch {
                abstract_text: Some(Some("user added".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // And then Zotero changes it too.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE itemDataValues SET value = 'V2' WHERE value = 'V1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE items SET dateModified = '2024-02-01 00:00:00' WHERE key = 'ABC12345'",
            [],
        )
        .unwrap();
    }

    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.conflicts.len(), 1, "conflict surfaced");
    assert_eq!(report.conflicts[0].external_id, "ABC12345");
    assert_eq!(report.conflicts[0].entity_type, EntityType::Reference);
}
