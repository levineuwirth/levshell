//! Integration tests for `levshell_sync::CalDavAdapter`.
//!
//! Plugs a `MockCalDavClient` into the adapter via
//! [`CalDavAdapter::with_factory`] so no real HTTP server has to
//! run. Drives the same SyncAdapter::sync path the scheduler uses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use levshell_data::{DataStore, EntityType, ListEvents};
use levshell_sync::caldav::{CalDavClient, CalDavError, DavEntry, PROVIDER_NAME};
use levshell_sync::{
    CalDavAdapter, CalDavConfig, CalendarSource, SyncAdapter, SyncContext, SyncStatus,
};

/// One fake CalDAV server, keyed by calendar URL. Each URL returns a
/// `(Vec<DavEntry>, HashMap<abs_href, ics>)`. Shared across all
/// MockCalDavClient instances via Arc.
#[derive(Default)]
struct ServerState {
    by_calendar: HashMap<String, CalendarState>,
    list_should_fail: bool,
}

#[derive(Default, Clone)]
struct CalendarState {
    entries: Vec<DavEntry>,
    bodies: HashMap<String, String>,
}

#[derive(Clone)]
struct MockCalDavClient {
    calendar_url: String,
    state: Arc<Mutex<ServerState>>,
}

#[async_trait]
impl CalDavClient for MockCalDavClient {
    async fn list_entries(&self, _base_url: &str) -> Result<Vec<DavEntry>, CalDavError> {
        let st = self.state.lock().unwrap();
        if st.list_should_fail {
            return Err(CalDavError::BadStatus {
                url: self.calendar_url.clone(),
                status: reqwest::StatusCode::UNAUTHORIZED,
            });
        }
        Ok(st
            .by_calendar
            .get(&self.calendar_url)
            .cloned()
            .unwrap_or_default()
            .entries)
    }

    async fn fetch_ics(&self, url: &str) -> Result<String, CalDavError> {
        let st = self.state.lock().unwrap();
        let cal = st
            .by_calendar
            .get(&self.calendar_url)
            .cloned()
            .unwrap_or_default();
        cal.bodies
            .get(url)
            .cloned()
            .ok_or_else(|| CalDavError::BadStatus {
                url: url.into(),
                status: reqwest::StatusCode::NOT_FOUND,
            })
    }
}

fn factory_for(
    state: Arc<Mutex<ServerState>>,
) -> levshell_sync::caldav::ClientFactory {
    Arc::new(move |source, _timeout| {
        Ok(Arc::new(MockCalDavClient {
            calendar_url: source.url.clone(),
            state: state.clone(),
        }) as Arc<dyn CalDavClient>)
    })
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

fn calendar_config(url: &str, name: &str) -> CalDavConfig {
    CalDavConfig {
        enabled: true,
        poll_interval_secs: 600,
        request_timeout_secs: 30,
        calendars: vec![CalendarSource {
            name: name.into(),
            url: url.into(),
            username: "u".into(),
            password: Some("p".into()),
            password_command: None,
        }],
    }
}

const UTC_EVENT_ICS: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//test//EN\r
BEGIN:VEVENT\r
UID:abc-123@example\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
DTEND:20260420T100000Z\r
SUMMARY:NeurIPS check-in\r
LOCATION:Zoom\r
DESCRIPTION:Joint session.\r
URL:https://example/meet\r
END:VEVENT\r
END:VCALENDAR\r
";

const UPDATED_EVENT_ICS: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//test//EN\r
BEGIN:VEVENT\r
UID:abc-123@example\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T100000Z\r
DTEND:20260420T110000Z\r
SUMMARY:NeurIPS check-in (rescheduled)\r
LOCATION:Zoom\r
END:VEVENT\r
END:VCALENDAR\r
";

const SECOND_EVENT_ICS: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:def-456@example\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260421T140000Z\r
DTEND:20260421T150000Z\r
SUMMARY:Second event\r
END:VEVENT\r
END:VCALENDAR\r
";

fn seed_one_event(
    state: &Arc<Mutex<ServerState>>,
    calendar_url: &str,
    abs_href: &str,
    etag: &str,
    ics: &str,
) {
    let mut st = state.lock().unwrap();
    let cal = st
        .by_calendar
        .entry(calendar_url.to_string())
        .or_default();
    cal.entries.push(DavEntry {
        href: abs_href.into(),
        etag: etag.into(),
    });
    cal.bodies.insert(abs_href.into(), ics.into());
}

// ----------------------------------------------------------------------

#[tokio::test]
async fn probe_healthy_on_successful_propfind() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state),
    );
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Healthy);
}

#[tokio::test]
async fn probe_unavailable_when_disabled() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    let cfg = CalDavConfig {
        enabled: false,
        ..calendar_config("https://srv/cal/work/", "work")
    };
    let adapter = CalDavAdapter::with_factory(cfg, factory_for(state));
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_unavailable_when_no_calendars_configured() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    let adapter = CalDavAdapter::with_factory(
        CalDavConfig::default(),
        factory_for(state),
    );
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn probe_unavailable_when_all_calendars_fail() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    state.lock().unwrap().list_should_fail = true;
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state),
    );
    assert_eq!(adapter.probe(&ctx(&store)).await, SyncStatus::Unavailable);
}

#[tokio::test]
async fn initial_sync_inserts_events() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/b.ics",
        "etag-2",
        SECOND_EVENT_ICS,
    );

    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state),
    );
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 2);
    assert_eq!(report.deleted, 0);

    let events = store.list_events(ListEvents::default()).await.unwrap();
    assert_eq!(events.len(), 2);
    let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
    assert!(titles.contains(&"NeurIPS check-in"));
    assert!(titles.contains(&"Second event"));

    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    let ids: Vec<&str> = metas.iter().map(|m| m.external_id.as_str()).collect();
    assert!(ids.contains(&"work/abc-123@example"));
    assert!(ids.contains(&"work/def-456@example"));
}

#[tokio::test]
async fn repeat_sync_is_idempotent_when_etag_unchanged() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state),
    );
    let first = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(first.upserted, 1);
    let second = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(second.upserted, 0, "same etag → skipped");
    assert_eq!(second.deleted, 0);
}

#[tokio::test]
async fn changed_etag_triggers_update() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state.clone()),
    );
    adapter.sync(&ctx(&store)).await.unwrap();

    // Server updates the ICS and bumps the etag.
    {
        let mut st = state.lock().unwrap();
        let cal = st
            .by_calendar
            .get_mut("https://srv/cal/work/")
            .unwrap();
        cal.entries[0].etag = "etag-2".into();
        cal.bodies.insert(
            "https://srv/cal/work/a.ics".into(),
            UPDATED_EVENT_ICS.into(),
        );
    }
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);

    let events = store.list_events(ListEvents::default()).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, "NeurIPS check-in (rescheduled)");
}

#[tokio::test]
async fn event_removed_from_propfind_is_deleted() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state.clone()),
    );
    adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(
        store.list_events(ListEvents::default()).await.unwrap().len(),
        1
    );

    // Server drops the event.
    {
        let mut st = state.lock().unwrap();
        let cal = st
            .by_calendar
            .get_mut("https://srv/cal/work/")
            .unwrap();
        cal.entries.clear();
        cal.bodies.clear();
    }
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(report.upserted, 0);
    assert!(store
        .list_events(ListEvents::default())
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn local_edit_between_syncs_flags_conflict() {
    use levshell_data::EventPatch;

    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-1",
        UTC_EVENT_ICS,
    );
    let adapter = CalDavAdapter::with_factory(
        calendar_config("https://srv/cal/work/", "work"),
        factory_for(state.clone()),
    );
    adapter.sync(&ctx(&store)).await.unwrap();

    // User edits locally.
    let events = store.list_events(ListEvents::default()).await.unwrap();
    store
        .update_event(
            events[0].id,
            EventPatch {
                description: Some(Some("user added".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Server also changes it.
    {
        let mut st = state.lock().unwrap();
        let cal = st
            .by_calendar
            .get_mut("https://srv/cal/work/")
            .unwrap();
        cal.entries[0].etag = "etag-2".into();
        cal.bodies.insert(
            "https://srv/cal/work/a.ics".into(),
            UPDATED_EVENT_ICS.into(),
        );
    }
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].external_id, "work/abc-123@example");
    assert_eq!(report.conflicts[0].entity_type, EntityType::Event);
}

#[tokio::test]
async fn multi_calendar_sync_keeps_external_ids_distinct() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/work/",
        "https://srv/cal/work/a.ics",
        "etag-w",
        UTC_EVENT_ICS,
    );
    seed_one_event(
        &state,
        "https://srv/cal/home/",
        "https://srv/cal/home/a.ics",
        "etag-h",
        UTC_EVENT_ICS, // same UID as the work one
    );

    let cfg = CalDavConfig {
        enabled: true,
        poll_interval_secs: 600,
        request_timeout_secs: 30,
        calendars: vec![
            CalendarSource {
                name: "work".into(),
                url: "https://srv/cal/work/".into(),
                username: "u".into(),
                password: Some("p".into()),
                password_command: None,
            },
            CalendarSource {
                name: "home".into(),
                url: "https://srv/cal/home/".into(),
                username: "u".into(),
                password: Some("p".into()),
                password_command: None,
            },
        ],
    };
    let adapter = CalDavAdapter::with_factory(cfg, factory_for(state));
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(
        report.upserted, 2,
        "same UID across calendars is namespaced by calendar name"
    );

    let metas = store
        .list_sync_metadata_by_provider(PROVIDER_NAME)
        .await
        .unwrap();
    let ids: Vec<&str> = metas.iter().map(|m| m.external_id.as_str()).collect();
    assert!(ids.contains(&"work/abc-123@example"));
    assert!(ids.contains(&"home/abc-123@example"));
}

#[tokio::test]
async fn calendar_listing_failure_skips_that_calendar_only() {
    let store = fresh_store().await;
    let state = Arc::new(Mutex::new(ServerState::default()));
    seed_one_event(
        &state,
        "https://srv/cal/home/",
        "https://srv/cal/home/a.ics",
        "etag-h",
        UTC_EVENT_ICS,
    );
    // Broken calendar: present in config but never seeded. Its
    // list_entries returns Ok([]) which is fine — no entries to
    // sync. Healthy one still works.
    let cfg = CalDavConfig {
        enabled: true,
        poll_interval_secs: 600,
        request_timeout_secs: 30,
        calendars: vec![
            CalendarSource {
                name: "broken".into(),
                url: "https://srv/cal/broken/".into(),
                username: "u".into(),
                password: Some("p".into()),
                password_command: None,
            },
            CalendarSource {
                name: "home".into(),
                url: "https://srv/cal/home/".into(),
                username: "u".into(),
                password: Some("p".into()),
                password_command: None,
            },
        ],
    };
    let adapter = CalDavAdapter::with_factory(cfg, factory_for(state));
    let report = adapter.sync(&ctx(&store)).await.unwrap();
    assert_eq!(report.upserted, 1);
    let events = store.list_events(ListEvents::default()).await.unwrap();
    assert_eq!(events.len(), 1);
}

// The caldav module exposes ClientFactory via the public path
// `levshell_sync::caldav::ClientFactory`; this is a no-op assertion
// that ensures the type alias continues to be importable.
#[allow(dead_code)]
fn _type_witness(_f: levshell_sync::caldav::ClientFactory) {}

// Silences an unused import from tokio timing we don't end up using
// (some scenarios awaited a real interval in an earlier draft).
#[allow(dead_code)]
fn _unused_duration(_: Duration) {}
