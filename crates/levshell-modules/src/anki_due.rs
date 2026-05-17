//! Anki review-due counter (spec §2.9.6 — "persistent badge in the bar
//! showing cards due").
//!
//! The AnkiConnect sync adapter imports card SRS state into the unified
//! `flashcards` table. This module is the *render* side: it polls
//! `DataStore::list_flashcards` with `due_before = now` on a slow cadence
//! and publishes a `WidgetUpdate` for the `anki-due` badge. It does not
//! talk to AnkiConnect — that isolation is the whole point of the unified
//! model (spec §5.1.1).
//!
//! State shape: `{ "due": <u32> }`. The badge is hidden by the context
//! engine / shell when `due == 0` (quiet until important, §1.3).

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_core::{Module, ModuleResult};
use levshell_data::{DataStore, ListFlashcards};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};

pub const ANKI_DUE_WIDGET_ID: &str = "anki-due";
pub const ANKI_DUE_WIDGET_TYPE: &str = "anki_due";
const MODULE_NAME: &str = "anki-due";

/// SRS due counts move on review completion (minutes–hours), so a tight
/// poll would be wasted work. 60s keeps the badge reasonably fresh.
const TICK_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// Count due cards in one query; bounds the row scan without loading the
/// whole deck. Anyone with >50k cards genuinely due has bigger problems
/// than an undercounted badge.
const DUE_QUERY_LIMIT: u32 = 50_000;

pub struct AnkiDueModule {
    store: DataStore,
    publisher: WidgetPublisher,
}

impl AnkiDueModule {
    pub fn new(store: DataStore, publisher: WidgetPublisher) -> Self {
        Self { store, publisher }
    }

    /// Shared query used by the module tick and by `ctl anki due-count`
    /// (via [`DataStore`] directly). Returns cards with `due_at <= now`.
    pub async fn due_count(store: &DataStore) -> Result<u32, levshell_data::DataError> {
        let due = store
            .list_flashcards(ListFlashcards {
                due_before: Some(Utc::now()),
                limit: Some(DUE_QUERY_LIMIT),
                ..Default::default()
            })
            .await?;
        Ok(due.len() as u32)
    }

    async fn refresh(&self) {
        let due = match Self::due_count(&self.store).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "anki-due: list_flashcards failed; skipping tick");
                return;
            }
        };
        let update = WidgetUpdate {
            widget_id: ANKI_DUE_WIDGET_ID.into(),
            widget_type: ANKI_DUE_WIDGET_TYPE.into(),
            state: serde_json::json!({ "due": due }),
            status: WidgetStatus::Normal,
            escalation: Default::default(),
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "anki-due: publish drop (channel full or closed)");
        }
    }
}

#[async_trait]
impl Module for AnkiDueModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(TICK_INTERVAL)
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
    use chrono::Duration;
    use levshell_data::NewFlashcard;
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::duplex;

    async fn store() -> DataStore {
        DataStore::open_in_memory().await.unwrap()
    }

    fn card(front: &str, due_at: chrono::DateTime<Utc>) -> NewFlashcard {
        NewFlashcard {
            front: front.into(),
            back: "answer".into(),
            linked_note_id: None,
            linked_ref_id: None,
            project_id: None,
            interval_days: 1.0,
            ease_factor: 2.5,
            due_at,
        }
    }

    #[tokio::test]
    async fn counts_only_due_cards() {
        let s = store().await;
        let now = Utc::now();
        // Due yesterday → counts.
        s.insert_flashcard(card("a", now - Duration::days(1)))
            .await
            .unwrap();
        // Due next week → does not count.
        s.insert_flashcard(card("b", now + Duration::days(7)))
            .await
            .unwrap();

        assert_eq!(AnkiDueModule::due_count(&s).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn start_publishes_a_widget_update() {
        let s = store().await;
        let (a, _b) = duplex(4096);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 16);
        let mut m = AnkiDueModule::new(s, task.publisher);
        // No cards → still publishes (due: 0), so the shell can tell
        // "0 due" from "never received".
        m.start().await.unwrap();
    }
}
