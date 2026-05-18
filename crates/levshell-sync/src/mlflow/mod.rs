//! MLflow experiment-tracker sync adapter (spec §2.10.7; §5.1.2 lists
//! W&B as the initial adapter and MLflow as the alternative — MLflow's
//! plain REST API is far less brittle than W&B's authenticated GraphQL,
//! so it's the one shipped first; a W&B sibling adapter is a clean later
//! addition).
//!
//! Imports MLflow *runs* into the unified `experiments` table — the
//! table the schema has always had but nothing populated until now.
//! Import-only. Tool-specific code stays confined here (spec §5.1.1):
//! every other module sees plain `Experiment` rows.
//!
//! Config — `~/.config/levshell/sync/mlflow.toml`:
//! ```toml
//! tracking_uri = "http://localhost:5000"
//! experiment_id = "0"               # MLflow experiment id
//! project_id   = "<levshell-project-uuid>"   # required: experiments
//!                                            # are NOT NULL on project
//! # token = "..."                   # optional bearer
//! ```
//! Without a parseable `project_id` the adapter probes Unavailable (a
//! clean degraded state, spec §5.2) rather than guessing a project.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use levshell_data::{
    EntityType, ExperimentPatch, ExperimentStatus, NewExperiment, SyncDirection, SyncMetadata,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::adapter::{
    Result, SyncAdapter, SyncContext, SyncError, SyncReport, SyncStatus,
};

pub const PROVIDER_NAME: &str = "mlflow";

#[derive(Debug, Clone, Deserialize)]
pub struct MlflowConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub tracking_uri: String,
    #[serde(default = "default_experiment_id")]
    pub experiment_id: String,
    /// levshell project UUID the runs attach to (experiments require a
    /// project). String here; parsed at probe/sync time.
    pub project_id: String,
    #[serde(default)]
    pub token: Option<String>,
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
fn default_experiment_id() -> String {
    "0".to_string()
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

impl MlflowConfig {
    pub fn load_from(path: &Path) -> std::result::Result<Self, MlflowConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| MlflowConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| MlflowConfigError::Toml {
            path: path.to_path_buf(),
            source: e,
        })
    }

    fn project_uuid(&self) -> Option<Uuid> {
        Uuid::parse_str(self.project_id.trim()).ok()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MlflowConfigError {
    #[error("reading mlflow config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing mlflow config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

// --- MLflow REST shapes (only the fields we consume) ---------------------

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    runs: Vec<RunObj>,
}
#[derive(Debug, Deserialize)]
struct RunObj {
    #[serde(default)]
    info: RunInfo,
    #[serde(default)]
    data: RunData,
}
#[derive(Debug, Default, Deserialize)]
struct RunInfo {
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    run_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    end_time: Option<i64>,
}
#[derive(Debug, Default, Deserialize)]
struct RunData {
    #[serde(default)]
    metrics: Vec<KeyVal>,
    #[serde(default)]
    tags: Vec<KeyVal>,
}
#[derive(Debug, Deserialize)]
struct KeyVal {
    key: String,
    #[serde(default)]
    value: serde_json::Value,
}

/// Map MLflow's run status string to our enum.
fn map_status(s: &str) -> ExperimentStatus {
    match s {
        "RUNNING" => ExperimentStatus::Running,
        "FINISHED" => ExperimentStatus::Completed,
        "FAILED" | "KILLED" => ExperimentStatus::Failed,
        _ => ExperimentStatus::Queued, // SCHEDULED / unknown
    }
}

fn ms_to_dt(ms: Option<i64>) -> Option<chrono::DateTime<Utc>> {
    ms.and_then(|m| Utc.timestamp_millis_opt(m).single())
}

fn metrics_json(metrics: &[KeyVal]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for kv in metrics {
        map.insert(kv.key.clone(), kv.value.clone());
    }
    serde_json::Value::Object(map)
}

fn git_hash(tags: &[KeyVal]) -> Option<String> {
    tags.iter()
        .find(|t| t.key == "mlflow.source.git.commit")
        .and_then(|t| t.value.as_str().map(str::to_owned))
}

pub struct MlflowAdapter {
    config: MlflowConfig,
    http: reqwest::Client,
}

impl MlflowAdapter {
    pub fn new(config: MlflowConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    async fn search_runs(&self) -> Result<Vec<RunObj>> {
        let url = format!(
            "{}/api/2.0/mlflow/runs/search",
            self.config.tracking_uri.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "experiment_ids": [self.config.experiment_id],
            "max_results": self.config.max_results,
            "run_view_type": "ACTIVE_ONLY",
        });
        let mut req = self.http.post(&url).json(&body);
        if let Some(tok) = &self.config.token {
            req = req.bearer_auth(tok);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| SyncError::Unavailable(format!("mlflow request: {e}")))?;
        if !resp.status().is_success() {
            return Err(SyncError::Unavailable(format!(
                "mlflow HTTP {}",
                resp.status()
            )));
        }
        let parsed: SearchResponse = resp
            .json()
            .await
            .map_err(|e| SyncError::Unavailable(format!("mlflow decode: {e}")))?;
        Ok(parsed.runs)
    }
}

#[async_trait]
impl SyncAdapter for MlflowAdapter {
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
        Duration::from_secs(self.config.timeout_secs.max(1))
    }

    async fn probe(&self, _ctx: &SyncContext) -> SyncStatus {
        if !self.config.enabled || self.config.project_uuid().is_none() {
            return SyncStatus::Unavailable;
        }
        // A cheap GET on the tracking server root; any response means
        // it's reachable (don't roll to Error on a transient blip).
        match self
            .http
            .get(self.config.tracking_uri.trim_end_matches('/'))
            .send()
            .await
        {
            Ok(_) => SyncStatus::Healthy,
            Err(_) => SyncStatus::Unavailable,
        }
    }

    async fn sync(&self, ctx: &SyncContext) -> Result<SyncReport> {
        let Some(project_id) = self.config.project_uuid() else {
            // Misconfigured — surface as Unavailable, never panic.
            return Err(SyncError::Unavailable(
                "mlflow: project_id is not a valid UUID".into(),
            ));
        };

        let runs = self.search_runs().await?;
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
            let external_id = run.info.run_id.clone();
            if external_id.is_empty() {
                continue;
            }
            let name = if run.info.run_name.is_empty() {
                external_id.clone()
            } else {
                run.info.run_name.clone()
            };
            let status = map_status(&run.info.status);
            let metrics = metrics_json(&run.data.metrics);
            let git = git_hash(&run.data.tags);
            let started = ms_to_dt(run.info.start_time);
            let completed = ms_to_dt(run.info.end_time);
            // Change detection: end_time||start_time||status as the hash.
            let hash = format!(
                "{}|{}|{}",
                run.info.status,
                run.info.start_time.unwrap_or(0),
                run.info.end_time.unwrap_or(0)
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
                            host: Some(self.config.tracking_uri.clone()),
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

    #[test]
    fn status_mapping() {
        assert_eq!(map_status("RUNNING"), ExperimentStatus::Running);
        assert_eq!(map_status("FINISHED"), ExperimentStatus::Completed);
        assert_eq!(map_status("FAILED"), ExperimentStatus::Failed);
        assert_eq!(map_status("KILLED"), ExperimentStatus::Failed);
        assert_eq!(map_status("SCHEDULED"), ExperimentStatus::Queued);
        assert_eq!(map_status("???"), ExperimentStatus::Queued);
    }

    #[test]
    fn metrics_and_git_extraction() {
        let metrics = vec![
            KeyVal { key: "loss".into(), value: serde_json::json!(0.12) },
            KeyVal { key: "acc".into(), value: serde_json::json!(0.97) },
        ];
        let j = metrics_json(&metrics);
        assert_eq!(j["loss"], 0.12);
        assert_eq!(j["acc"], 0.97);

        let tags = vec![KeyVal {
            key: "mlflow.source.git.commit".into(),
            value: serde_json::json!("abc123"),
        }];
        assert_eq!(git_hash(&tags).as_deref(), Some("abc123"));
        assert_eq!(git_hash(&[]), None);
    }

    #[test]
    fn search_response_parses_runs() {
        let body = serde_json::json!({
            "runs": [{
                "info": { "run_id": "r1", "run_name": "baseline",
                          "status": "RUNNING", "start_time": 1700000000000_i64 },
                "data": { "metrics": [{ "key": "loss", "value": 0.5 }],
                          "tags": [{ "key": "mlflow.source.git.commit",
                                     "value": "deadbeef" }] }
            }]
        });
        let parsed: SearchResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.runs.len(), 1);
        assert_eq!(parsed.runs[0].info.run_id, "r1");
        assert_eq!(parsed.runs[0].info.status, "RUNNING");
        assert_eq!(parsed.runs[0].data.metrics[0].key, "loss");
    }

    #[test]
    fn dormant_without_valid_project_uuid() {
        let cfg = MlflowConfig {
            enabled: true,
            tracking_uri: "http://localhost:5000".into(),
            experiment_id: "0".into(),
            project_id: "not-a-uuid".into(),
            token: None,
            poll_secs: 300,
            timeout_secs: 30,
            max_results: 100,
        };
        assert!(cfg.project_uuid().is_none());
    }
}
