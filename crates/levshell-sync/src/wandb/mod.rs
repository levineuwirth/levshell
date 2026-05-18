//! Weights & Biases experiment-tracker sync adapter (spec §5.1.2 — W&B
//! is the *initial* adapter the design names; [`crate::mlflow`] shipped
//! first only because MLflow's plain REST is less brittle than W&B's
//! authenticated GraphQL. This is the promised sibling).
//!
//! Imports W&B *runs* into the unified `experiments` table — the same
//! table the MLflow adapter populates, so every downstream module sees
//! plain `Experiment` rows regardless of which tracker the user runs
//! (spec §5.1.1: tool-specific code stays confined to the adapter).
//! Import-only.
//!
//! W&B has no REST runs endpoint; the public API is a single GraphQL
//! POST authenticated with HTTP Basic (`api:<key>`). A self-hosted
//! server overrides `base_url`.
//!
//! Config — `~/.config/levshell/sync/wandb.toml`:
//! ```toml
//! entity     = "my-team"     # W&B entity (user or team)
//! project    = "my-project"  # W&B project name
//! api_key    = "..."         # required: W&B API key
//! project_id = "<levshell-project-uuid>"  # required: experiments are
//!                                         # NOT NULL on project
//! # base_url = "https://api.wandb.ai"     # self-hosted override
//! ```
//! Missing `api_key`, blank `entity`/`project`, or an unparseable
//! `project_id` makes the adapter probe Unavailable (a clean degraded
//! state, spec §5.2) rather than guessing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use levshell_data::{
    EntityType, ExperimentPatch, ExperimentStatus, NewExperiment, SyncDirection, SyncMetadata,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::adapter::{Result, SyncAdapter, SyncContext, SyncError, SyncReport, SyncStatus};

pub const PROVIDER_NAME: &str = "wandb";

#[derive(Debug, Clone, Deserialize)]
pub struct WandbConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    pub entity: String,
    pub project: String,
    pub api_key: String,
    /// levshell project UUID the runs attach to (experiments require a
    /// project). String here; parsed at probe/sync time.
    pub project_id: String,
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

fn default_enabled() -> bool {
    true
}
fn default_base_url() -> String {
    "https://api.wandb.ai".to_string()
}
fn default_poll_secs() -> u64 {
    300
}
fn default_timeout_secs() -> u64 {
    30
}
fn default_max_results() -> u32 {
    100
}

/// Hard ceiling for the configured request timeout — same defense as
/// the MLflow adapter: `probe()` gates the whole loop, so a
/// misconfigured huge `timeout_secs` must not let one request stall.
const MAX_TIMEOUT_SECS: u64 = 120;

/// Reject a response whose declared length exceeds this. `max_results`
/// is config-bounded but reqwest has no default body cap; a misbehaving
/// server (or a wildly large `max_results`) must not OOM the daemon.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// W&B caps `runs(first:)` at 500 server-side; clamp so a large
/// `max_results` doesn't produce a query the API rejects outright.
const MAX_PAGE: u32 = 500;

impl WandbConfig {
    pub fn load_from(path: &Path) -> std::result::Result<Self, WandbConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| WandbConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| WandbConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Configured request timeout, clamped to `[1, MAX_TIMEOUT_SECS]`.
    fn effective_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.clamp(1, MAX_TIMEOUT_SECS))
    }

    fn project_uuid(&self) -> Option<Uuid> {
        Uuid::parse_str(self.project_id.trim()).ok()
    }

    /// All the config that must be present for the adapter to do
    /// anything useful. Anything missing → probe Unavailable.
    fn is_usable(&self) -> bool {
        self.enabled
            && self.project_uuid().is_some()
            && !self.entity.trim().is_empty()
            && !self.project.trim().is_empty()
            && !self.api_key.trim().is_empty()
    }

    fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base_url.trim_end_matches('/'))
    }

    fn page_size(&self) -> u32 {
        self.max_results.clamp(1, MAX_PAGE)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WandbConfigError {
    #[error("reading wandb config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing wandb config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

// --- W&B GraphQL shapes (only the fields we consume) --------------------

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: Option<RespData>,
    #[serde(default)]
    errors: Option<Vec<GqlError>>,
}
#[derive(Debug, Deserialize)]
struct GqlError {
    #[serde(default)]
    message: String,
}
#[derive(Debug, Deserialize)]
struct RespData {
    #[serde(default)]
    project: Option<ProjectObj>,
}
#[derive(Debug, Deserialize)]
struct ProjectObj {
    #[serde(default)]
    runs: RunConnection,
}
#[derive(Debug, Default, Deserialize)]
struct RunConnection {
    #[serde(default)]
    edges: Vec<RunEdge>,
}
#[derive(Debug, Deserialize)]
struct RunEdge {
    #[serde(default)]
    node: RunNode,
}
#[derive(Debug, Default, Deserialize)]
struct RunNode {
    /// Immutable short run id (e.g. `3a1b2c4d`) — our `external_id`.
    #[serde(default)]
    name: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default, rename = "heartbeatAt")]
    heartbeat_at: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    /// W&B returns this as a *JSON-encoded string*, not an object.
    #[serde(default, rename = "summaryMetrics")]
    summary_metrics: Option<String>,
}

/// Map W&B's run state string to our enum. W&B states:
/// `running`/`pending` (live), `finished` (ok), `crashed`/`failed`/
/// `killed` (bad), `preempted` (requeued by scheduler).
fn map_status(s: &str) -> ExperimentStatus {
    match s.to_ascii_lowercase().as_str() {
        "running" | "pending" => ExperimentStatus::Running,
        "finished" => ExperimentStatus::Completed,
        "crashed" | "failed" | "killed" => ExperimentStatus::Failed,
        _ => ExperimentStatus::Queued, // preempted / unknown
    }
}

/// W&B timestamps come back ISO-8601, sometimes with a `Z`/offset and
/// sometimes a bare naive datetime (treated as UTC). Try both; an
/// unparseable value is dropped rather than failing the whole run.
fn parse_ts(s: &Option<String>) -> Option<DateTime<Utc>> {
    let raw = s.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return Utc.from_utc_datetime(&naive).into();
        }
    }
    None
}

/// W&B's `summaryMetrics` is a JSON *string*; decode it to an object,
/// falling back to `{}` on anything non-object or unparseable so a
/// weird payload never fails the run.
fn parse_summary(s: &Option<String>) -> serde_json::Value {
    let empty = || serde_json::Value::Object(serde_json::Map::new());
    let Some(raw) = s.as_deref() else {
        return empty();
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        _ => empty(),
    }
}

pub struct WandbAdapter {
    config: WandbConfig,
    http: reqwest::Client,
}

impl WandbAdapter {
    pub fn new(config: WandbConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.effective_timeout())
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    fn runs_query(&self) -> serde_json::Value {
        // `order:"-createdAt"` so a config-bounded page is the most
        // recent runs, not an arbitrary slice.
        let query = "query Runs($entity:String!,$project:String!,$first:Int!){\
            project(name:$project,entityName:$entity){\
              runs(first:$first,order:\"-createdAt\"){\
                edges{node{\
                  name displayName state createdAt heartbeatAt commit summaryMetrics\
                }}\
              }\
            }\
          }";
        serde_json::json!({
            "query": query,
            "variables": {
                "entity": self.config.entity.trim(),
                "project": self.config.project.trim(),
                "first": self.config.page_size(),
            }
        })
    }

    async fn fetch_runs(&self) -> Result<Vec<RunNode>> {
        let resp = self
            .http
            .post(self.config.graphql_url())
            // W&B auth: HTTP Basic, username `api`, password = key.
            .basic_auth("api", Some(self.config.api_key.trim()))
            .json(&self.runs_query())
            .send()
            .await
            .map_err(|e| SyncError::Unavailable(format!("wandb request: {e}")))?;
        if !resp.status().is_success() {
            return Err(SyncError::Unavailable(format!(
                "wandb HTTP {}",
                resp.status()
            )));
        }
        if let Some(len) = resp.content_length() {
            if len > MAX_BODY_BYTES {
                return Err(SyncError::Unavailable(format!(
                    "wandb response too large: {len} bytes (cap {MAX_BODY_BYTES})"
                )));
            }
        }
        let parsed: GraphQlResponse = resp
            .json()
            .await
            .map_err(|e| SyncError::Unavailable(format!("wandb decode: {e}")))?;

        if let Some(errs) = parsed.errors.filter(|e| !e.is_empty()) {
            // A GraphQL error (bad entity/project, revoked key) is an
            // External fault, not Unavailable — the engine surfaces it
            // but keeps retrying so a fixed config recovers.
            let msg = errs
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SyncError::External(format!("wandb graphql: {msg}")));
        }
        Ok(parsed
            .data
            .and_then(|d| d.project)
            .map(|p| p.runs.edges.into_iter().map(|e| e.node).collect())
            .unwrap_or_default())
    }
}

#[async_trait]
impl SyncAdapter for WandbAdapter {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn entity_types(&self) -> Vec<EntityType> {
        vec![EntityType::Experiment]
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_secs.max(30))
    }

    fn timeout(&self) -> Duration {
        self.config.effective_timeout()
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        if !self.config.is_usable() {
            return SyncStatus::Unavailable;
        }
        // Cheap reachability check: any HTTP response from the GraphQL
        // endpoint means W&B is up. Don't roll to Error on a transient
        // network blip (mirrors the MLflow adapter's lenient probe).
        match self
            .http
            .post(self.config.graphql_url())
            .basic_auth("api", Some(self.config.api_key.trim()))
            .json(&serde_json::json!({ "query": "query{viewer{id}}" }))
            .send()
            .await
        {
            Ok(_) => SyncStatus::Healthy,
            Err(_) => SyncStatus::Unavailable,
        }
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let Some(project_id) = self.config.project_uuid() else {
            return Err(SyncError::Unavailable(
                "wandb: project_id is not a valid UUID".into(),
            ));
        };

        let runs = self.fetch_runs().await?;
        let mut report = SyncReport::default();

        let existing = ctx
            .store
            .list_sync_metadata_by_provider(PROVIDER_NAME)
            .await?;
        let mut by_external: HashMap<String, SyncMetadata> = existing
            .into_iter()
            .map(|m| (m.external_id.clone(), m))
            .collect();

        for run in runs {
            let external_id = run.name.clone();
            if external_id.is_empty() {
                continue;
            }
            let name = if run.display_name.trim().is_empty() {
                external_id.clone()
            } else {
                run.display_name.clone()
            };
            let status = map_status(&run.state);
            let metrics = parse_summary(&run.summary_metrics);
            let git = run.commit.filter(|c| !c.trim().is_empty());
            let started = parse_ts(&run.created_at);
            let completed = parse_ts(&run.heartbeat_at);
            // Change detection: state + the two timestamps. heartbeat
            // advances while a run is live and freezes when it ends, so
            // this catches both progress and completion.
            let hash = format!(
                "{}|{}|{}",
                run.state,
                run.created_at.as_deref().unwrap_or(""),
                run.heartbeat_at.as_deref().unwrap_or("")
            );

            match by_external.remove(&external_id) {
                Some(prev) => {
                    if prev.sync_hash.as_deref() == Some(hash.as_str()) {
                        continue; // unchanged
                    }
                    ctx.store
                        .update_experiment(
                            prev.entity_id,
                            ExperimentPatch {
                                name: Some(name),
                                status: Some(status),
                                git_hash: Some(git),
                                metrics: Some(metrics),
                                started_at: Some(started),
                                completed_at: Some(completed),
                                ..Default::default()
                            },
                        )
                        .await?;
                    upsert_meta(ctx, prev.entity_id, &external_id, &hash).await?;
                    report.upserted += 1;
                }
                None => {
                    let exp = ctx
                        .store
                        .insert_experiment(NewExperiment {
                            name,
                            project_id,
                            hypothesis: None,
                            status,
                            host: Some(format!(
                                "{}/{}",
                                self.config.entity.trim(),
                                self.config.project.trim()
                            )),
                            git_hash: git,
                            config: None,
                            notes: None,
                        })
                        .await?;
                    if metrics != serde_json::json!({}) || started.is_some() {
                        ctx.store
                            .update_experiment(
                                exp.id,
                                ExperimentPatch {
                                    metrics: Some(metrics),
                                    started_at: Some(started),
                                    completed_at: Some(completed),
                                    ..Default::default()
                                },
                            )
                            .await?;
                    }
                    upsert_meta(ctx, exp.id, &external_id, &hash).await?;
                    report.upserted += 1;
                }
            }
        }

        Ok(report)
    }
}

async fn upsert_meta(
    ctx: &SyncContext,
    entity_id: Uuid,
    external_id: &str,
    hash: &str,
) -> Result<()> {
    ctx.store
        .set_sync_metadata(SyncMetadata {
            entity_id,
            entity_type: EntityType::Experiment,
            provider: PROVIDER_NAME.into(),
            external_id: external_id.to_owned(),
            last_synced_at: Utc::now(),
            sync_direction: SyncDirection::ImportOnly,
            sync_hash: Some(hash.to_owned()),
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(project_id: &str) -> WandbConfig {
        WandbConfig {
            enabled: true,
            base_url: "https://api.wandb.ai".into(),
            entity: "team".into(),
            project: "proj".into(),
            api_key: "k".into(),
            project_id: project_id.into(),
            poll_secs: 300,
            timeout_secs: 30,
            max_results: 100,
        }
    }

    #[test]
    fn status_mapping_is_case_insensitive() {
        assert_eq!(map_status("running"), ExperimentStatus::Running);
        assert_eq!(map_status("PENDING"), ExperimentStatus::Running);
        assert_eq!(map_status("finished"), ExperimentStatus::Completed);
        assert_eq!(map_status("crashed"), ExperimentStatus::Failed);
        assert_eq!(map_status("failed"), ExperimentStatus::Failed);
        assert_eq!(map_status("killed"), ExperimentStatus::Failed);
        assert_eq!(map_status("preempted"), ExperimentStatus::Queued);
        assert_eq!(map_status("???"), ExperimentStatus::Queued);
    }

    #[test]
    fn summary_metrics_decodes_json_string_only_when_object() {
        let s = Some(r#"{"loss":0.12,"acc":0.97}"#.to_string());
        let j = parse_summary(&s);
        assert_eq!(j["loss"], 0.12);
        assert_eq!(j["acc"], 0.97);
        // Non-object / garbage / absent → empty object, never a panic.
        assert_eq!(parse_summary(&Some("[1,2,3]".into())), serde_json::json!({}));
        assert_eq!(parse_summary(&Some("not json".into())), serde_json::json!({}));
        assert_eq!(parse_summary(&None), serde_json::json!({}));
    }

    #[test]
    fn timestamp_parsing_handles_rfc3339_and_naive() {
        assert!(parse_ts(&Some("2024-01-02T03:04:05Z".into())).is_some());
        assert!(parse_ts(&Some("2024-01-02T03:04:05".into())).is_some());
        assert!(parse_ts(&Some("2024-01-02T03:04:05.123456".into())).is_some());
        assert!(parse_ts(&Some("garbage".into())).is_none());
        assert!(parse_ts(&None).is_none());
        assert!(parse_ts(&Some("  ".into())).is_none());
    }

    #[test]
    fn graphql_response_parses_runs_and_surfaces_errors() {
        let body = serde_json::json!({
            "data": { "project": { "runs": { "edges": [
                { "node": {
                    "name": "abc123", "displayName": "baseline",
                    "state": "running", "createdAt": "2024-01-02T03:04:05Z",
                    "commit": "deadbeef",
                    "summaryMetrics": "{\"loss\":0.5}"
                }}
            ] } } }
        });
        let parsed: GraphQlResponse = serde_json::from_value(body).unwrap();
        let runs: Vec<RunNode> = parsed
            .data
            .unwrap()
            .project
            .unwrap()
            .runs
            .edges
            .into_iter()
            .map(|e| e.node)
            .collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "abc123");
        assert_eq!(runs[0].state, "running");
        assert_eq!(parse_summary(&runs[0].summary_metrics)["loss"], 0.5);

        let errbody = serde_json::json!({
            "errors": [{ "message": "project not found" }]
        });
        let e: GraphQlResponse = serde_json::from_value(errbody).unwrap();
        assert_eq!(e.errors.unwrap()[0].message, "project not found");
    }

    #[test]
    fn unusable_without_required_config() {
        assert!(cfg("not-a-uuid").project_uuid().is_none());
        assert!(!cfg("not-a-uuid").is_usable());

        let good = Uuid::now_v7().to_string();
        assert!(cfg(&good).is_usable());

        let mut no_key = cfg(&good);
        no_key.api_key = "  ".into();
        assert!(!no_key.is_usable(), "blank api_key → unusable");

        let mut disabled = cfg(&good);
        disabled.enabled = false;
        assert!(!disabled.is_usable());

        let mut no_entity = cfg(&good);
        no_entity.entity = "".into();
        assert!(!no_entity.is_usable());
    }

    #[test]
    fn page_size_is_clamped() {
        let mut c = cfg(&Uuid::now_v7().to_string());
        c.max_results = 0;
        assert_eq!(c.page_size(), 1);
        c.max_results = 100_000;
        assert_eq!(c.page_size(), MAX_PAGE);
        c.max_results = 250;
        assert_eq!(c.page_size(), 250);
    }

    #[test]
    fn graphql_url_joins_cleanly() {
        let mut c = cfg(&Uuid::now_v7().to_string());
        c.base_url = "https://api.wandb.ai/".into();
        assert_eq!(c.graphql_url(), "https://api.wandb.ai/graphql");
        c.base_url = "http://localhost:8080".into();
        assert_eq!(c.graphql_url(), "http://localhost:8080/graphql");
    }
}
