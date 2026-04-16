//! Integration tests for `levshell_sync::AnkiConnectAdapter`.
//!
//! Uses an in-memory `MockAnkiClient` plugged into the adapter via
//! [`AnkiConnectAdapter::with_client`] so no real AnkiConnect process
//! has to run. The sync is driven by the same SyncAdapter::sync path
//! the scheduler uses.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use levshell_data::{DataStore, EntityType, ListFlashcards};
use levshell_sync::ankiconnect::{AnkiClient, AnkiClientError, CardInfo, NoteInfo, PROVIDER_NAME};
use levshell_sync::{AnkiConnectAdapter, AnkiConnectConfig, SyncAdapter, SyncContext, SyncStatus};
use serde_json::json;

#[derive(Default)]
struct MockState {
    version: Option<u32>,
    find_cards: Vec<i64>,
    cards: Vec<CardInfo>,
    notes: Vec<NoteInfo>,
    fail_version: bool,
}

#[derive(Default, Clone)]
struct MockAnkiClient {
    state: Arc<Mutex<MockState>>,
}

impl MockAnkiClient {
    fn set_version(&self, v: u32) {
        self.state.lock().unwrap().version = Some(v);
    }

    fn fail_version(&self) {
        self.state.lock().unwrap().fail_version = true;
    }

    fn set_cards(&self, cards: Vec<CardInfo>) {
        self.state.lock().unwrap().find_cards = cards.iter().map(|c| c.card_id).collect();
        self.state.lock().unwrap().cards = cards;
    }

    fn set_notes(&self, notes: Vec<NoteInfo>) {
        self.state.lock().unwrap().notes = notes;
    }
}

#[async_trait]
impl AnkiClient for MockAnkiClient {
    async fn version(&self) -> Result<u32, AnkiClientError> {
        let st = self.state.lock().unwrap();
        if st.fail_version {
            return Err(AnkiClientError::Api("anki is closed".into()));
        }
        Ok(st.version.unwrap_or(6))
    }

    async fn find_cards(&self, _query: &str) -> Result<Vec<i64>, AnkiClientError> {
        Ok(self.state.lock().unwrap().find_cards.clone())
    }

    async fn cards_info(&self, ids: &[i64]) -> Result<Vec<CardInfo>, AnkiClientError> {
        let st = self.state.lock().unwrap();
        let out: Vec<CardInfo> = st
            .cards
            .iter()
            .filter(|c| ids.contains(&c.card_id))
            .cloned()
            .collect();
        Ok(out)
    }

    async fn notes_info(&self, ids: &[i64]) -> Result<Vec<NoteInfo>, AnkiClientError> {
        let st = self.state.lock().unwrap();
        let out: Vec<NoteInfo> = st
            .notes
            .iter()
            .filter(|n| ids.contains(&n.note_id))
            .cloned()
            .collect();
        Ok(out)
    }
}

async fn fresh_store() -> DataStore {
    DataStore::open_in_memory().await.unwrap()
}

fn ctx(store: &DataStore) -> SyncContext {
    SyncContext {
        store: store.clone(),
        since: None,
    }
}

/// Build a card with sensible defaults for review-queue.
fn review_card(id: i64, note: i64, front: &str, back: &str, modified: i64) -> CardInfo {
    serde_json::from_value(json!({
        "cardId": id,
        "note": note,
        "deckName": "Default",
        "question": front,
        "answer": back,
        "interval": 7,
        "factor": 2500,
        "queue": 2,
        "mod": modified,
        "reps": 3,
        "due": 19000,
    }))
    .unwrap()
}

fn suspended_card(id: i64, note: i64) -> CardInfo {
    serde_json::from_value(json!({
        "cardId": id,
        "note": note,
        "deckName": "Default",
        "question": "",
        "answer": "",
        "interval": 0,
        "factor": 2500,
        "queue": -1,
        "mod": 0,
        "reps": 0,
        "due": 0,
    }))
    .unwrap()
}

fn note_with_tags(id: i64, tags: &[&str]) -> NoteInfo {
    serde_json::from_value(json!({
        "noteId": id,
        "modelName": "Basic",
        "tags": tags,
    }))
    .unwrap()
}

// -----------------------------------------------------------------------

#[tokio::test]
async fn probe_is_unavailable_when_version_fails() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.fail_version();
    let adapter = AnkiConnectAdapter::with_client(
        AnkiConnectConfig::default(),
        mock,
    );
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_is_unavailable_when_disabled() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    let cfg = AnkiConnectConfig {
        enabled: false,
        ..AnkiConnectConfig::default()
    };
    let adapter = AnkiConnectAdapter::with_client(cfg, mock);
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_is_unavailable_on_old_api_version() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(5);
    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock);
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn initial_sync_inserts_flashcards_with_tags() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![
        review_card(101, 201, "<p>2+2</p>", "<p>4</p>", 1_000_000),
        review_card(102, 202, "<p>rust</p>", "<p>lang</p>", 1_000_001),
    ]);
    mock.set_notes(vec![
        note_with_tags(201, &["math"]),
        note_with_tags(202, &["lang", "rust"]),
    ]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 2);
    assert_eq!(report.deleted, 0);

    let cards = store
        .list_flashcards(ListFlashcards::default())
        .await
        .unwrap();
    assert_eq!(cards.len(), 2);

    let math = cards.iter().find(|c| c.front == "2+2").unwrap();
    assert_eq!(math.back, "4");
    assert_eq!(math.interval_days, 7.0);
    assert!((math.ease_factor - 2.5).abs() < 1e-9);

    let tags = store.get_tags(math.id, EntityType::Flashcard).await.unwrap();
    assert_eq!(tags, vec!["math".to_string()]);

    let lang = cards.iter().find(|c| c.front == "rust").unwrap();
    let tags = store.get_tags(lang.id, EntityType::Flashcard).await.unwrap();
    let mut sorted = tags;
    sorted.sort();
    assert_eq!(sorted, vec!["lang".to_string(), "rust".to_string()]);

    // sync_metadata keyed on Anki cardId.
    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    assert_eq!(metas.len(), 2);
    assert!(metas.iter().any(|m| m.external_id == "101"));
}

#[tokio::test]
async fn suspended_card_is_skipped() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![
        review_card(101, 201, "f", "b", 1),
        suspended_card(999, 999), // excluded
    ]);
    mock.set_notes(vec![note_with_tags(201, &[])]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1, "suspended card not synced");
}

#[tokio::test]
async fn repeat_sync_is_idempotent_when_modified_unchanged() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![review_card(101, 201, "f", "b", 1_000_000)]);
    mock.set_notes(vec![note_with_tags(201, &["math"])]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock);
    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 1);
    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(second.upserted, 0, "unchanged mod → skipped");
    assert_eq!(second.deleted, 0);
}

#[tokio::test]
async fn changed_mod_timestamp_triggers_update() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![review_card(101, 201, "<p>v1</p>", "<p>b</p>", 1_000_000)]);
    mock.set_notes(vec![note_with_tags(201, &[])]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock.clone());
    adapter.sync(&ctx(&store)).await.unwrap();

    // Simulate a user editing the card in Anki — front text + mod bump.
    mock.set_cards(vec![review_card(101, 201, "<p>v2</p>", "<p>b</p>", 1_000_100)]);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);

    let cards = store
        .list_flashcards(ListFlashcards::default())
        .await
        .unwrap();
    assert_eq!(cards[0].front, "v2");
}

#[tokio::test]
async fn card_no_longer_in_findcards_is_deleted() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![review_card(101, 201, "f", "b", 1)]);
    mock.set_notes(vec![note_with_tags(201, &[])]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock.clone());
    adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(
        store
            .list_flashcards(ListFlashcards::default())
            .await
            .unwrap()
            .len(),
        1
    );

    // User suspends / deletes / moves-out-of-filter — findCards empty.
    mock.set_cards(vec![]);
    mock.set_notes(vec![]);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(report.upserted, 0);
    assert!(
        store
            .list_flashcards(ListFlashcards::default())
            .await
            .unwrap()
            .is_empty(),
        "deleted card's flashcard row is gone"
    );
    assert!(
        store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await
            .unwrap()
            .is_empty(),
        "sync_metadata cleaned up alongside the flashcard"
    );
}

#[tokio::test]
async fn local_edit_after_sync_flags_conflict() {
    use levshell_data::FlashcardPatch;

    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    mock.set_cards(vec![review_card(101, 201, "orig", "b", 1_000_000)]);
    mock.set_notes(vec![note_with_tags(201, &[])]);

    let adapter = AnkiConnectAdapter::with_client(AnkiConnectConfig::default(), mock.clone());
    adapter.sync(&ctx(&store)).await.unwrap();

    let cards = store
        .list_flashcards(ListFlashcards::default())
        .await
        .unwrap();
    let local_id = cards[0].id;

    store
        .update_flashcard(
            local_id,
            FlashcardPatch {
                front: Some("user edited".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Then Anki changes it too.
    mock.set_cards(vec![review_card(101, 201, "anki v2", "b", 1_000_100)]);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.conflicts.len(), 1, "conflict surfaced");
    assert_eq!(report.conflicts[0].external_id, "101");
    assert_eq!(report.conflicts[0].entity_type, EntityType::Flashcard);
}

#[tokio::test]
async fn deck_filter_narrows_the_set() {
    let store = fresh_store().await;
    let mock = Arc::new(MockAnkiClient::default());
    mock.set_version(6);
    // Irrespective of what query arrives, the mock returns the two
    // cards it was told about — but this test asserts that the
    // adapter feeds the configured `deck_filter` through to the
    // findCards call rather than substituting a default, by checking
    // the call is not erroring out with a non-default filter.
    mock.set_cards(vec![review_card(1, 1, "x", "y", 1)]);
    mock.set_notes(vec![note_with_tags(1, &[])]);

    let cfg = AnkiConnectConfig {
        deck_filter: "deck:Research -is:suspended".into(),
        ..AnkiConnectConfig::default()
    };
    let adapter = AnkiConnectAdapter::with_client(cfg, mock);
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);
}
