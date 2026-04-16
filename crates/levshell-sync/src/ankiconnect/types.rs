//! AnkiConnect response DTOs.
//!
//! We deserialize just the fields the adapter needs. AnkiConnect
//! includes plenty of extras (css, templates, media references) that
//! the unified data model has no slot for; serde's default
//! `deny_unknown_fields = false` lets us quietly drop them.

use std::collections::HashMap;

use serde::Deserialize;

/// Single card as returned by `cardsInfo`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CardInfo {
    /// Anki's 13-digit card ID. Used as the sync_metadata external_id.
    #[serde(rename = "cardId")]
    pub card_id: i64,

    /// Anki note this card belongs to. `notesInfo` keyed on this gives
    /// us tags and full field values.
    #[serde(rename = "note")]
    pub note_id: i64,

    #[serde(rename = "deckName")]
    pub deck_name: String,

    /// Rendered HTML of the card's question side. We strip HTML at
    /// the adapter boundary before writing to `flashcards.front`.
    pub question: String,

    /// Rendered HTML of the answer.
    pub answer: String,

    /// Days until next review for queue ∈ {2, 3}; learning-step
    /// seconds for queue == 1; ordinal position for queue == 0.
    /// Always a signed integer in AnkiConnect's response.
    pub interval: i32,

    /// Scheduling ease × 1000. Levshell stores it as a `f64` so we
    /// divide by 1000 at translation time (Anki's 2500 → 2.5).
    pub factor: i64,

    /// Queue discriminant:
    /// - `0` new
    /// - `1` learning
    /// - `2` review
    /// - `3` day-learning (relearn)
    /// - `-1` suspended, `-2` user-buried, `-3` scheduler-buried
    pub queue: i32,

    /// Modification timestamp (seconds since epoch). We use this as
    /// the sync_hash so card edits propagate without content diffs.
    #[serde(rename = "mod")]
    pub modified: i64,

    /// Number of times reviewed.
    pub reps: i32,

    /// See [`Self::interval`] for the semantics — `due` carries the
    /// same unit, interpreted per-queue.
    pub due: i64,
}

impl CardInfo {
    /// Whether the card is in a queue we want to surface. Suspended
    /// and buried cards are intentionally excluded; the user treats
    /// them as "not part of my review stream."
    pub fn is_syncable(&self) -> bool {
        self.queue >= 0
    }
}

/// Single note as returned by `notesInfo`. The adapter only uses
/// `tags` from this — the card's `question`/`answer` come from the
/// `cardsInfo` call.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct NoteInfo {
    #[serde(rename = "noteId")]
    pub note_id: i64,

    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default, rename = "modelName")]
    pub model_name: String,

    /// AnkiConnect returns fields as `{ "Front": {"value": "...", "order": 0}, ... }`.
    /// We don't use the per-field detail in v1; it's kept on the DTO
    /// so future features (inline card creation, field-aware search)
    /// can reach it without another sync pass.
    #[serde(default)]
    pub fields: HashMap<String, FieldValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct FieldValue {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub order: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_realistic_cards_info_item() {
        let raw = json!({
            "cardId": 1498938915123_i64,
            "note": 1498938915000_i64,
            "deckName": "Default",
            "modelName": "Basic",
            "fieldOrder": 0,
            "question": "<p>Front</p>",
            "answer": "<p>Back</p>",
            "css": ".card { ... }",
            "interval": 21,
            "factor": 2500,
            "queue": 2,
            "mod": 1640995200,
            "reps": 5,
            "lapses": 0,
            "due": 19000,
            "type": 2,
            "nextReviews": ["<1m", "<6m", "1d", "4d"],
            "flags": 0,
        });
        let card: CardInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(card.card_id, 1498938915123);
        assert_eq!(card.note_id, 1498938915000);
        assert_eq!(card.interval, 21);
        assert_eq!(card.queue, 2);
        assert!(card.is_syncable());
    }

    #[test]
    fn suspended_card_is_not_syncable() {
        let raw = json!({
            "cardId": 1, "note": 2, "deckName": "d",
            "question": "", "answer": "",
            "interval": 0, "factor": 0, "queue": -1,
            "mod": 0, "reps": 0, "due": 0,
        });
        let card: CardInfo = serde_json::from_value(raw).unwrap();
        assert!(!card.is_syncable());
    }

    #[test]
    fn deserializes_notes_info_item_with_tags() {
        let raw = json!({
            "noteId": 1498938915000_i64,
            "modelName": "Basic",
            "tags": ["rust", "levshell"],
            "fields": {
                "Front": {"value": "Front text", "order": 0},
                "Back": {"value": "Back text", "order": 1},
            },
        });
        let note: NoteInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(note.tags, vec!["rust".to_string(), "levshell".to_string()]);
        assert_eq!(note.fields["Front"].value, "Front text");
    }
}
