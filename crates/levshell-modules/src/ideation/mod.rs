//! Ideation engine module (spec §2.2, §6.2 item 5).
//!
//! At a tick-aligned cadence, the engine rolls a Bernoulli trial with
//! probability `tick / lambda` (approximating a Poisson process at
//! `lambda ≈ 45 min` per spec). When the trial fires, [`selector`]
//! picks a [`Nudge`] and the module publishes
//! [`Event::NudgeDelivered`] on the bus.
//!
//! Delivery layers (freedesktop notification rendering, shell overlay,
//! escalation to the command palette) subscribe to that event rather
//! than being hard-wired here. The v1 engine itself only logs INFO and
//! publishes — keeping the selection algorithm decoupled from the
//! rendering path is spec §3.5.1's "quiet by default" in practice.
//!
//! Without a [`ProjectRegistry`] (e.g. no `~/.config/levshell/projects/`
//! configured) the module starts healthy but every tick is a no-op —
//! there's nothing to nudge about.

pub mod config;
pub mod nudge;
pub mod selector;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use levshell_core::{Event, EventKind, Module, ModuleError, ModuleResult};
use levshell_data::{DataStore, EntityType, ListNotes, ListReferences};
use levshell_ipc::WidgetPublisher;
use levshell_projects::ProjectRegistry;
use rand::rngs::StdRng;
use rand::SeedableRng;
use tokio::sync::Mutex;

pub use config::{IdeationConfig, IdeationConfigError, NudgeWeights};
pub use nudge::{Nudge, NudgeKind};
pub use selector::{NudgeContext, NudgeSelector, RecentEntity};

pub const MODULE_NAME: &str = "ideation";

/// The ideation engine. Constructed with a data store and an optional
/// project registry; missing the registry degrades every tick to a
/// no-op but keeps the module's health Normal (the spec treats "no
/// projects configured" as a valid daily-driver state).
pub struct IdeationModule {
    store: DataStore,
    projects: Option<ProjectRegistry>,
    bus: levshell_core::EventBus,
    /// The widget publisher is kept so a future v2 can push a nudge
    /// widget (e.g. a subtle "new suggestion" pill in the bar) without
    /// changing the module signature. v1 delivers via bus events only.
    _publisher: WidgetPublisher,
    config: IdeationConfig,
    rng: Arc<Mutex<StdRng>>,
}

impl IdeationModule {
    pub fn new(
        bus: levshell_core::EventBus,
        publisher: WidgetPublisher,
        store: DataStore,
        projects: Option<ProjectRegistry>,
    ) -> Self {
        Self::with_config(bus, publisher, store, projects, IdeationConfig::default())
    }

    pub fn with_config(
        bus: levshell_core::EventBus,
        publisher: WidgetPublisher,
        store: DataStore,
        projects: Option<ProjectRegistry>,
        config: IdeationConfig,
    ) -> Self {
        Self {
            store,
            projects,
            bus,
            _publisher: publisher,
            config,
            rng: Arc::new(Mutex::new(StdRng::from_entropy())),
        }
    }

    /// Seed the RNG deterministically. Tests and integration harnesses
    /// call this to make tick behavior reproducible.
    pub fn with_seeded_rng(mut self, seed: u64) -> Self {
        self.rng = Arc::new(Mutex::new(StdRng::seed_from_u64(seed)));
        self
    }

    pub fn config(&self) -> &IdeationConfig {
        &self.config
    }

    /// Load config from `ideation.toml` in the given directory. Missing
    /// file → defaults. A parse failure is logged and also falls back
    /// to defaults so a broken TOML never wedges the daemon.
    pub fn load_config_from_dir(dir: &std::path::Path) -> IdeationConfig {
        let path: PathBuf = dir.join("ideation.toml");
        if !path.exists() {
            return IdeationConfig::default();
        }
        match IdeationConfig::load_from(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "ideation config parse failed; using defaults"
                );
                IdeationConfig::default()
            }
        }
    }

    /// Pure-ish tick body: builds the snapshot, asks the selector, and
    /// publishes. Separated so tests can drive one tick without the
    /// runner loop around it.
    async fn run_tick(&self) -> ModuleResult<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let Some(registry) = self.projects.as_ref() else {
            return Ok(());
        };

        let projects = registry.list().await;
        if projects.is_empty() {
            return Ok(());
        }

        let recent_entities = self.load_recent_entities().await.map_err(|e| {
            ModuleError::failed(format!("ideation: failed to load recent entities: {e}"))
        })?;

        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &recent_entities,
            now: Utc::now(),
            config: &self.config,
        };

        let mut rng = self.rng.lock().await;
        if !selector::should_fire_this_tick(&ctx, &mut *rng) {
            return Ok(());
        }
        let Some(nudge) = NudgeSelector::new().select(&ctx, &mut *rng) else {
            tracing::debug!(module = MODULE_NAME, "fire rolled but no candidate available");
            return Ok(());
        };
        drop(rng);

        tracing::info!(
            module = MODULE_NAME,
            project_id = %nudge.project_id,
            kind = %nudge.kind.as_str(),
            title = %nudge.title,
            "nudge delivered"
        );
        self.bus.publish(Event::NudgeDelivered {
            project_id: nudge.project_id,
            kind: nudge.kind.as_str().into(),
            title: nudge.title,
        });
        Ok(())
    }

    /// Fetch recent Notes and References (within the config's
    /// `recent_seed_hours`) and enrich each with its tag set. Used by
    /// the cross-connection selector. Tag-fetches are per-row for
    /// simplicity — v1 keeps the recency window short enough that
    /// N+1 doesn't matter.
    async fn load_recent_entities(&self) -> Result<Vec<RecentEntity>, levshell_data::DataError> {
        let horizon = Utc::now()
            - chrono::Duration::hours(self.config.recent_seed_hours.max(1) as i64);

        let mut out = Vec::new();

        let notes = self
            .store
            .list_notes(ListNotes {
                limit: Some(200),
                ..Default::default()
            })
            .await?;
        for note in notes {
            if note.updated_at < horizon {
                continue;
            }
            let tags = self.store.get_tags(note.id, EntityType::Note).await?;
            out.push(RecentEntity {
                id: note.id,
                entity_type: EntityType::Note,
                title: note.title,
                project_id: note.project_id,
                tags,
                updated_at: note.updated_at,
            });
        }

        let refs = self
            .store
            .list_references(ListReferences {
                limit: Some(200),
                ..Default::default()
            })
            .await?;
        for r in refs {
            if r.updated_at < horizon {
                continue;
            }
            let tags = self.store.get_tags(r.id, EntityType::Reference).await?;
            out.push(RecentEntity {
                id: r.id,
                entity_type: EntityType::Reference,
                title: r.title,
                project_id: r.project_id,
                tags,
                updated_at: r.updated_at,
            });
        }
        Ok(out)
    }

    /// Force one tick synchronously from outside the runner. Useful in
    /// integration tests that want to exercise the real selection
    /// path without spinning a tokio interval. Respects `enabled`.
    pub async fn tick_for_test(&self) -> ModuleResult<()> {
        self.run_tick().await
    }
}

#[async_trait]
impl Module for IdeationModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn tick_interval(&self) -> Option<Duration> {
        Some(self.config.tick_interval())
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        // Subscribe broadly, per spec §3.4 ("the ideation engine
        // subscribes broadly to maintain situational awareness"). v1
        // doesn't act on these — it just keeps the subscription open
        // so v2 can react to sync completions etc. without a wire
        // change.
        vec![
            EventKind::WorkspaceChanged,
            EventKind::SyncCompleted,
            EventKind::DataStoreUpdated,
        ]
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.run_tick().await
    }
}
