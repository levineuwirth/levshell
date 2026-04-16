//! Thin AnkiConnect JSON-RPC client.
//!
//! AnkiConnect speaks a simple request/response protocol over HTTP:
//!
//! ```json
//! POST /
//! { "action": "findCards", "version": 6, "params": {"query": "is:due"} }
//!
//! HTTP 200
//! { "result": [1498938915123, ...], "error": null }
//! ```
//!
//! Every action gets `version: 6`; responses always have `result` and
//! `error` fields (exactly one is non-null). We expose a [`AnkiClient`]
//! trait so the adapter depends on a seam the tests can mock without
//! spinning up an HTTP server.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::types::{CardInfo, NoteInfo};

pub const ANKICONNECT_API_VERSION: u32 = 6;

#[derive(Debug, Error)]
pub enum AnkiClientError {
    #[error("http error contacting AnkiConnect at {endpoint}: {source}")]
    Http {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("AnkiConnect returned an error: {0}")]
    Api(String),

    #[error("AnkiConnect response missing expected shape: {0}")]
    Malformed(String),

    #[error("AnkiConnect API version mismatch: expected {expected}, got {got}")]
    VersionMismatch { expected: u32, got: u32 },
}

/// Trait the adapter depends on. Real impl is [`AnkiConnectHttpClient`];
/// unit tests plug in a mock that returns canned values.
#[async_trait]
pub trait AnkiClient: Send + Sync {
    /// Returns the AnkiConnect API version. Used as the liveness probe
    /// before every sync pass.
    async fn version(&self) -> Result<u32, AnkiClientError>;

    /// Run a `findCards` query. Returns the matching card IDs.
    async fn find_cards(&self, query: &str) -> Result<Vec<i64>, AnkiClientError>;

    /// Batched `cardsInfo` lookup. Callers pass the full ID list; the
    /// implementation chunks as needed.
    async fn cards_info(&self, ids: &[i64]) -> Result<Vec<CardInfo>, AnkiClientError>;

    /// Batched `notesInfo` lookup. Used for tags + precise field
    /// retrieval (the `fields` embedded in `cardsInfo` is per-card
    /// and already includes them, but `notesInfo` gives us the tags).
    async fn notes_info(&self, ids: &[i64]) -> Result<Vec<NoteInfo>, AnkiClientError>;
}

/// Production [`AnkiClient`]. Wraps a `reqwest::Client` with the
/// adapter's configured endpoint, timeout, and optional API key.
pub struct AnkiConnectHttpClient {
    http: Client,
    endpoint: String,
    api_key: Option<String>,
}

impl AnkiConnectHttpClient {
    pub fn new(
        endpoint: impl Into<String>,
        timeout: Duration,
        api_key: Option<String>,
    ) -> Result<Self, AnkiClientError> {
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|source| AnkiClientError::Http {
                endpoint: "<builder>".into(),
                source,
            })?;
        Ok(Self {
            http,
            endpoint: endpoint.into(),
            api_key,
        })
    }

    async fn request<P, R>(&self, action: &str, params: P) -> Result<R, AnkiClientError>
    where
        P: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        #[derive(Serialize)]
        struct Envelope<'a, P> {
            action: &'a str,
            version: u32,
            params: P,
            #[serde(skip_serializing_if = "Option::is_none")]
            key: Option<String>,
        }

        let envelope = Envelope {
            action,
            version: ANKICONNECT_API_VERSION,
            params,
            key: self.api_key.clone(),
        };

        let raw = self
            .http
            .post(&self.endpoint)
            .json(&envelope)
            .send()
            .await
            .map_err(|source| AnkiClientError::Http {
                endpoint: self.endpoint.clone(),
                source,
            })?
            .error_for_status()
            .map_err(|source| AnkiClientError::Http {
                endpoint: self.endpoint.clone(),
                source,
            })?
            .json::<Value>()
            .await
            .map_err(|source| AnkiClientError::Http {
                endpoint: self.endpoint.clone(),
                source,
            })?;

        parse_response::<R>(&raw)
    }
}

/// Empty params marker. AnkiConnect's `version` action takes no
/// params; we still have to send an empty object so the server parses
/// the envelope.
#[derive(Serialize)]
struct EmptyParams {}

#[async_trait]
impl AnkiClient for AnkiConnectHttpClient {
    async fn version(&self) -> Result<u32, AnkiClientError> {
        self.request::<_, u32>("version", EmptyParams {}).await
    }

    async fn find_cards(&self, query: &str) -> Result<Vec<i64>, AnkiClientError> {
        #[derive(Serialize)]
        struct Params<'a> {
            query: &'a str,
        }
        self.request::<_, Vec<i64>>("findCards", Params { query })
            .await
    }

    async fn cards_info(&self, ids: &[i64]) -> Result<Vec<CardInfo>, AnkiClientError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(Serialize)]
        struct Params<'a> {
            cards: &'a [i64],
        }
        self.request::<_, Vec<CardInfo>>("cardsInfo", Params { cards: ids })
            .await
    }

    async fn notes_info(&self, ids: &[i64]) -> Result<Vec<NoteInfo>, AnkiClientError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(Serialize)]
        struct Params<'a> {
            notes: &'a [i64],
        }
        self.request::<_, Vec<NoteInfo>>("notesInfo", Params { notes: ids })
            .await
    }
}

/// Pull `result` out of an AnkiConnect envelope. The server returns
/// `{result, error}` with exactly one non-null — we surface the
/// `error` string if present, and deserialize `result` into `R`
/// otherwise.
fn parse_response<R: for<'de> Deserialize<'de>>(raw: &Value) -> Result<R, AnkiClientError> {
    if let Some(err) = raw.get("error").and_then(Value::as_str) {
        return Err(AnkiClientError::Api(err.to_string()));
    }
    let result = raw
        .get("result")
        .ok_or_else(|| AnkiClientError::Malformed("missing `result` field".into()))?;
    serde_json::from_value(result.clone())
        .map_err(|e| AnkiClientError::Malformed(format!("deserializing `result`: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_response_extracts_result() {
        let raw = json!({ "result": [1, 2, 3], "error": null });
        let v: Vec<i64> = parse_response(&raw).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn parse_response_surfaces_error_field() {
        let raw = json!({ "result": null, "error": "deck was not found" });
        let err = parse_response::<Value>(&raw).unwrap_err();
        match err {
            AnkiClientError::Api(msg) => assert!(msg.contains("deck was not found")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_response_rejects_missing_result() {
        let raw = json!({ "error": null });
        let err = parse_response::<Value>(&raw).unwrap_err();
        assert!(matches!(err, AnkiClientError::Malformed(_)));
    }
}
