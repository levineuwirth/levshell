//! [`ProjectRegistry`] — indexed access to projects + attach/detach.
//!
//! The registry is built once at daemon startup from a directory of TOML
//! files via [`ProjectRegistry::load_from_dir`]. Each file is upserted
//! into the `projects` table (by name) and indexed in-memory alongside
//! runtime metadata (git repos, SSH hosts, workspace names, accent color,
//! and auto-attach tags).
//!
//! # Upsert semantics
//!
//! When a TOML file's `name` matches an existing project row:
//! - The DB row's `status`, `description`, and `open_questions` are
//!   updated to reflect the TOML. The user treats the TOML as
//!   authoritative for the fields it declares.
//! - The project's `id` does not change, so attached entities keep their
//!   link.
//!
//! When no matching row exists, a new project is inserted with a fresh
//! UUID v7 id.
//!
//! # Attach / detach
//!
//! Attach sets `entity.project_id = Some(project_id)` on the target row.
//! Detach sets it to `None`. The registry dispatches to the right
//! per-entity-type CRUD method on [`DataStore`]. Experiments cannot be
//! detached (their `project_id` is `NOT NULL` in the schema — they
//! always belong to exactly one project).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use levshell_config::{watch_config_dir, ConfigChange, ConfigWatcher, WatcherError};
use levshell_core::EventBus;
use levshell_data::{
    DataStore, EntityType, EventPatch, FlashcardPatch, ListProjects, NewProject, NotePatch,
    Project, ProjectPatch, ProjectStatus, ReferencePatch, TaskPatch,
};
use thiserror::Error;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{load_project_file, ProjectFile};

/// Errors the registry produces. Most operations only touch the in-memory
/// index + the data store, so the only sources are `DataError` (already
/// wrapped) and a couple of lookup failures.
#[derive(Debug, Error)]
pub enum ProjectRegistryError {
    #[error("data store error: {0}")]
    Data(#[from] levshell_data::DataError),

    /// A ctl client referenced a project that no longer exists. Also
    /// returned when [`ProjectRegistry::attach`] is given an unknown
    /// project UUID.
    #[error("project not found: {0}")]
    UnknownProject(String),

    /// A ctl client passed an [`EntityType`] whose table has no
    /// `project_id` column (currently: `Project` itself). Experiments
    /// also land here because they mandatorily have a project_id and
    /// cannot be "detached".
    #[error("entity type {0:?} cannot be attached to / detached from a project")]
    UnattachableType(EntityType),

    /// Generic not-found for attach/detach target entities.
    #[error("entity not found")]
    EntityNotFound,
}

/// Runtime metadata a project has beyond what lives in the `projects`
/// table. Populated from TOML and indexed in-memory; not persisted.
#[derive(Debug, Clone, Default)]
pub struct ProjectMetadata {
    pub tags: Vec<String>,
    pub git_repos: Vec<PathBuf>,
    pub ssh_hosts: Vec<String>,
    pub workspace_names: Vec<String>,
    pub accent_color: Option<String>,
}

/// The union of the DB-backed [`Project`] row and the TOML-only
/// [`ProjectMetadata`]. What the registry hands out to callers.
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub project: Project,
    pub metadata: ProjectMetadata,
}

/// Indexed, thread-safe access to the project set.
///
/// The registry is cheap to clone — internally an `Arc` around the index.
/// Pass clones freely to modules / ctl handlers that need lookup access.
#[derive(Clone)]
pub struct ProjectRegistry {
    store: DataStore,
    // Event bus is held for future work (publishing ProjectActivated,
    // ProjectAttached events so the context engine can react). Not yet
    // used in v1.
    #[allow(dead_code)]
    bus: EventBus,
    by_id: Arc<RwLock<HashMap<Uuid, ProjectEntry>>>,
    by_name: Arc<RwLock<HashMap<String, Uuid>>>,
}

impl ProjectRegistry {
    /// Build a registry from a directory of TOML files. Each file is
    /// parsed, upserted into the data store, and indexed. Missing or
    /// unreadable directory → empty registry (logged as a debug).
    pub async fn load_from_dir(
        store: DataStore,
        bus: EventBus,
        dir: &Path,
    ) -> Result<Self, ProjectRegistryError> {
        let files = crate::config::load_projects_from_dir(dir);
        Self::load_from_files(store, bus, files).await
    }

    /// Build a registry from an explicit list of parsed TOML files. Used
    /// by tests to bypass the filesystem walk.
    pub async fn load_from_files(
        store: DataStore,
        bus: EventBus,
        files: Vec<ProjectFile>,
    ) -> Result<Self, ProjectRegistryError> {
        let registry = Self {
            store,
            bus,
            by_id: Arc::new(RwLock::new(HashMap::new())),
            by_name: Arc::new(RwLock::new(HashMap::new())),
        };
        for file in files {
            registry.upsert_from_file(file).await?;
        }
        Ok(registry)
    }

    /// Empty registry — used by the daemon when no projects directory is
    /// configured. Callers can still create projects via
    /// [`Self::upsert_from_file`] or attach/detach by UUID.
    pub fn empty(store: DataStore, bus: EventBus) -> Self {
        Self {
            store,
            bus,
            by_id: Arc::new(RwLock::new(HashMap::new())),
            by_name: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Upsert one parsed TOML file: update the matching DB row (by name)
    /// or insert a new one, then index it in memory.
    pub async fn upsert_from_file(
        &self,
        file: ProjectFile,
    ) -> Result<ProjectEntry, ProjectRegistryError> {
        let existing = self
            .store
            .list_projects(ListProjects::default())
            .await?
            .into_iter()
            .find(|p| p.name == file.name);

        let project = match existing {
            Some(p) => {
                self.store
                    .update_project(
                        p.id,
                        ProjectPatch {
                            status: file.status,
                            description: file.description.clone(),
                            open_questions: Some(file.open_questions.clone()),
                            ..Default::default()
                        },
                    )
                    .await?
            }
            None => {
                self.store
                    .insert_project(NewProject {
                        name: file.name.clone(),
                        status: file.status.unwrap_or(ProjectStatus::Active),
                        description: file.description.clone().unwrap_or_default(),
                        open_questions: file.open_questions.clone(),
                    })
                    .await?
            }
        };

        let metadata = ProjectMetadata {
            tags: file.tags,
            git_repos: file.git_repos,
            ssh_hosts: file.ssh_hosts,
            workspace_names: file.workspace_names,
            accent_color: file.accent_color,
        };

        let entry = ProjectEntry {
            project: project.clone(),
            metadata,
        };
        {
            let mut by_id = self.by_id.write().await;
            let mut by_name = self.by_name.write().await;
            by_id.insert(project.id, entry.clone());
            by_name.insert(project.name.clone(), project.id);
        }
        tracing::info!(
            id = %project.id,
            name = %project.name,
            "project registered"
        );
        Ok(entry)
    }

    /// Snapshot of every indexed project, sorted by name.
    pub async fn list(&self) -> Vec<ProjectEntry> {
        let by_id = self.by_id.read().await;
        let mut entries: Vec<ProjectEntry> = by_id.values().cloned().collect();
        entries.sort_by(|a, b| a.project.name.cmp(&b.project.name));
        entries
    }

    pub async fn get(&self, id: Uuid) -> Option<ProjectEntry> {
        self.by_id.read().await.get(&id).cloned()
    }

    pub async fn find_by_name(&self, name: &str) -> Option<ProjectEntry> {
        let id = self.by_name.read().await.get(name).copied()?;
        self.get(id).await
    }

    /// Resolve a user-supplied string to a project id: accepts either
    /// the exact project name OR a UUID-v7 string. Returns
    /// `UnknownProject` when neither matches.
    pub async fn resolve(&self, identifier: &str) -> Result<Uuid, ProjectRegistryError> {
        if let Some(entry) = self.find_by_name(identifier).await {
            return Ok(entry.project.id);
        }
        if let Ok(id) = Uuid::parse_str(identifier) {
            if self.by_id.read().await.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(ProjectRegistryError::UnknownProject(identifier.to_string()))
    }

    /// Returns the project with the largest tag overlap against `tags`,
    /// or `None` if no project has any overlap. Ties break by
    /// lexicographic project-name order for determinism.
    pub async fn find_by_tags(&self, tags: &[String]) -> Option<ProjectEntry> {
        let by_id = self.by_id.read().await;
        let mut best: Option<(&ProjectEntry, usize)> = None;
        for entry in by_id.values() {
            let overlap = entry
                .metadata
                .tags
                .iter()
                .filter(|t| tags.iter().any(|candidate| candidate == *t))
                .count();
            if overlap == 0 {
                continue;
            }
            match best {
                None => best = Some((entry, overlap)),
                Some((current, current_overlap)) => {
                    if overlap > current_overlap
                        || (overlap == current_overlap
                            && entry.project.name < current.project.name)
                    {
                        best = Some((entry, overlap));
                    }
                }
            }
        }
        best.map(|(e, _)| e.clone())
    }

    /// Returns the project whose `workspace_names` contains `workspace`,
    /// or `None`.
    pub async fn find_by_workspace(&self, workspace: &str) -> Option<ProjectEntry> {
        let by_id = self.by_id.read().await;
        by_id
            .values()
            .find(|e| e.metadata.workspace_names.iter().any(|w| w == workspace))
            .cloned()
    }

    /// Bind an entity to a project. Dispatches to the right per-entity
    /// CRUD method to set `project_id`. The entity type must be one of
    /// `Note`, `Reference`, `Flashcard`, `Event`, or `Task`.
    pub async fn attach(
        &self,
        entity_type: EntityType,
        entity_id: Uuid,
        project_id: Uuid,
    ) -> Result<(), ProjectRegistryError> {
        // Verify the project exists so we don't leave a dangling FK
        // reference (sqlite would catch this too, but a domain error is
        // a better response to the ctl client).
        if !self.by_id.read().await.contains_key(&project_id) {
            return Err(ProjectRegistryError::UnknownProject(project_id.to_string()));
        }
        self.set_entity_project(entity_type, entity_id, Some(project_id))
            .await
    }

    /// Unbind an entity from its current project (if any).
    /// `Experiment` cannot be detached — its project_id column is
    /// `NOT NULL`. Returns [`ProjectRegistryError::UnattachableType`].
    pub async fn detach(
        &self,
        entity_type: EntityType,
        entity_id: Uuid,
    ) -> Result<(), ProjectRegistryError> {
        if matches!(entity_type, EntityType::Experiment) {
            return Err(ProjectRegistryError::UnattachableType(entity_type));
        }
        self.set_entity_project(entity_type, entity_id, None).await
    }

    async fn set_entity_project(
        &self,
        entity_type: EntityType,
        entity_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<(), ProjectRegistryError> {
        match entity_type {
            EntityType::Note => {
                self.store
                    .update_note(
                        entity_id,
                        NotePatch {
                            project_id: Some(project_id),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            EntityType::Reference => {
                self.store
                    .update_reference(
                        entity_id,
                        ReferencePatch {
                            project_id: Some(project_id),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            EntityType::Flashcard => {
                self.store
                    .update_flashcard(
                        entity_id,
                        FlashcardPatch {
                            project_id: Some(project_id),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            EntityType::Event => {
                self.store
                    .update_event(
                        entity_id,
                        EventPatch {
                            project_id: Some(project_id),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            EntityType::Task => {
                self.store
                    .update_task(
                        entity_id,
                        TaskPatch {
                            project_id: Some(project_id),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            EntityType::Project | EntityType::Experiment => {
                return Err(ProjectRegistryError::UnattachableType(entity_type));
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ProjectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectRegistry")
            .field("store", &"DataStore")
            .finish_non_exhaustive()
    }
}

impl ProjectRegistry {
    /// Watch `dir` for changes to `*.toml` project files and
    /// automatically upsert changes into the registry without a daemon
    /// restart (spec §3.9). Returns a handle that owns the OS-level
    /// watch plus the background task draining its events; dropping
    /// it stops both.
    ///
    /// Behaviour on each event:
    /// - **File created or modified**: parse and upsert. If parsing
    ///   fails (malformed TOML, missing `name`), log a warning and
    ///   leave the registry as-is.
    /// - **File removed**: log an info-level message and keep the
    ///   corresponding DB row. A filesystem deletion is not taken as
    ///   authorization to destroy user data — the user may be renaming,
    ///   moving across vaults, or having an editor-crash race. The
    ///   user can remove a project explicitly via the data layer
    ///   (future CtlRequest).
    pub fn spawn_watcher(
        &self,
        dir: &Path,
    ) -> Result<ProjectRegistryWatcher, WatcherError> {
        let (watcher, mut rx) = watch_config_dir(dir)?;
        let registry = self.clone();
        let handle = tokio::spawn(async move {
            while let Some(change) = rx.recv().await {
                match change {
                    ConfigChange::Upserted(path) => match load_project_file(&path) {
                        Ok(file) => {
                            if let Err(e) = registry.upsert_from_file(file).await {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "project hot-reload: upsert failed"
                                );
                            } else {
                                tracing::info!(
                                    path = %path.display(),
                                    "project hot-reload: upserted"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "project hot-reload: failed to parse; registry unchanged"
                            );
                        }
                    },
                    ConfigChange::Removed(path) => {
                        tracing::info!(
                            path = %path.display(),
                            "project hot-reload: file removed (DB row preserved)"
                        );
                    }
                }
            }
            tracing::debug!("project hot-reload: watcher channel closed");
        });
        Ok(ProjectRegistryWatcher {
            _watcher: watcher,
            task: handle,
        })
    }
}

/// Handle to the project-registry hot-reload watcher. Holds the
/// underlying OS watch and the async task draining its events. Drop to
/// stop both; explicit [`Self::shutdown`] waits for the task to exit.
pub struct ProjectRegistryWatcher {
    _watcher: ConfigWatcher,
    task: JoinHandle<()>,
}

impl ProjectRegistryWatcher {
    /// Abort the hot-reload task and wait for it to exit. Call this on
    /// clean daemon shutdown; `drop` does the same thing but without
    /// awaiting the join.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

impl std::fmt::Debug for ProjectRegistryWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectRegistryWatcher").finish_non_exhaustive()
    }
}
