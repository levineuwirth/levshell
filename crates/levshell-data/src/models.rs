//! Entity model structs and the enums they reference.
//!
//! Each struct mirrors the columns of its SQL table verbatim. Fields stored
//! as JSON in the schema (e.g. `open_questions`) appear here as their
//! deserialized Rust shape; the conversion happens at the rusqlite boundary
//! inside the per-entity ops modules. The polymorphic `EntityType` enum
//! mirrors the `CHECK` constraint on `entity_tags.entity_type`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::DataError;

// ---------------------------------------------------------------------------
// Entity type enum (polymorphic key for tags / relations / sync_metadata)
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Project,
    Note,
    /// Note: stored as `"ref"` in the database to match the schema's CHECK
    /// constraint and avoid colliding with the SQL keyword `references`.
    #[serde(rename = "ref")]
    Reference,
    Flashcard,
    Event,
    Task,
    Experiment,
}

impl EntityType {
    pub const fn as_str(self) -> &'static str {
        match self {
            EntityType::Project => "project",
            EntityType::Note => "note",
            EntityType::Reference => "ref",
            EntityType::Flashcard => "flashcard",
            EntityType::Event => "event",
            EntityType::Task => "task",
            EntityType::Experiment => "experiment",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "project" => EntityType::Project,
            "note" => EntityType::Note,
            "ref" => EntityType::Reference,
            "flashcard" => EntityType::Flashcard,
            "event" => EntityType::Event,
            "task" => EntityType::Task,
            "experiment" => EntityType::Experiment,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "entity_type",
                    value: other.to_string(),
                })
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    #[default]
    Active,
    Simmering,
    Blocked,
    WritingUp,
    Complete,
}

impl ProjectStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Simmering => "simmering",
            ProjectStatus::Blocked => "blocked",
            ProjectStatus::WritingUp => "writing_up",
            ProjectStatus::Complete => "complete",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "active" => ProjectStatus::Active,
            "simmering" => ProjectStatus::Simmering,
            "blocked" => ProjectStatus::Blocked,
            "writing_up" => ProjectStatus::WritingUp,
            "complete" => ProjectStatus::Complete,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "project.status",
                    value: other.to_string(),
                })
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub status: ProjectStatus,
    pub description: String,
    pub open_questions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub status: ProjectStatus,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<ProjectStatus>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub open_questions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListProjects {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub status: Option<ProjectStatus>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Note
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: Uuid,
    pub title: String,
    pub content: String,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewNote {
    pub title: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotePatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// Wrapped in `Option<Option<…>>` so the patch can express three cases:
    /// `None` = leave alone, `Some(None)` = unset, `Some(Some(id))` = set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListNotes {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Reference (literature)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub citekey: String,
    pub abstract_text: Option<String>,
    pub pdf_path: Option<String>,
    pub reading_progress: Option<f64>,
    pub annotations: Vec<String>,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewReference {
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub doi: Option<String>,
    pub citekey: String,
    #[serde(default)]
    pub abstract_text: Option<String>,
    #[serde(default)]
    pub pdf_path: Option<String>,
    #[serde(default)]
    pub reading_progress: Option<f64>,
    #[serde(default)]
    pub annotations: Vec<String>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferencePatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    #[serde(default)]
    pub year: Option<Option<i32>>,
    #[serde(default)]
    pub venue: Option<Option<String>>,
    #[serde(default)]
    pub doi: Option<Option<String>>,
    #[serde(default)]
    pub citekey: Option<String>,
    #[serde(default)]
    pub abstract_text: Option<Option<String>>,
    #[serde(default)]
    pub pdf_path: Option<Option<String>>,
    #[serde(default)]
    pub reading_progress: Option<Option<f64>>,
    #[serde(default)]
    pub annotations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListReferences {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Flashcard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flashcard {
    pub id: Uuid,
    pub front: String,
    pub back: String,
    pub linked_note_id: Option<Uuid>,
    pub linked_ref_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub interval_days: f64,
    pub ease_factor: f64,
    pub due_at: DateTime<Utc>,
    pub review_count: i32,
    pub last_reviewed: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewFlashcard {
    pub front: String,
    pub back: String,
    #[serde(default)]
    pub linked_note_id: Option<Uuid>,
    #[serde(default)]
    pub linked_ref_id: Option<Uuid>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default = "default_interval")]
    pub interval_days: f64,
    #[serde(default = "default_ease")]
    pub ease_factor: f64,
    pub due_at: DateTime<Utc>,
}

fn default_interval() -> f64 {
    1.0
}
fn default_ease() -> f64 {
    2.5
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlashcardPatch {
    #[serde(default)]
    pub front: Option<String>,
    #[serde(default)]
    pub back: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_note_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_ref_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub interval_days: Option<f64>,
    #[serde(default)]
    pub ease_factor: Option<f64>,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub review_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reviewed: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListFlashcards {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub due_before: Option<DateTime<Utc>>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Event (calendar)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub title: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub project_id: Option<Uuid>,
    pub recurrence: Option<String>,
    pub reminders: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    pub title: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub recurrence: Option<String>,
    #[serde(default)]
    pub reminders: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub start_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Option<String>>,
    #[serde(default)]
    pub reminders: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListEvents {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Active,
    Done,
    Cancelled,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Active => "active",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "pending" => TaskStatus::Pending,
            "active" => TaskStatus::Active,
            "done" => TaskStatus::Done,
            "cancelled" => TaskStatus::Cancelled,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "task.status",
                    value: other.to_string(),
                })
            }
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Urgent,
}

impl TaskPriority {
    pub const fn as_str(self) -> &'static str {
        match self {
            TaskPriority::Low => "low",
            TaskPriority::Medium => "medium",
            TaskPriority::High => "high",
            TaskPriority::Urgent => "urgent",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "low" => TaskPriority::Low,
            "medium" => TaskPriority::Medium,
            "high" => TaskPriority::High,
            "urgent" => TaskPriority::Urgent,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "task.priority",
                    value: other.to_string(),
                })
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: Option<TaskPriority>,
    pub due_at: Option<DateTime<Utc>>,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Option<TaskPriority>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListTasks {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub status: Option<TaskStatus>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Experiment
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
}

impl ExperimentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExperimentStatus::Queued => "queued",
            ExperimentStatus::Running => "running",
            ExperimentStatus::Completed => "completed",
            ExperimentStatus::Failed => "failed",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "queued" => ExperimentStatus::Queued,
            "running" => ExperimentStatus::Running,
            "completed" => ExperimentStatus::Completed,
            "failed" => ExperimentStatus::Failed,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "experiment.status",
                    value: other.to_string(),
                })
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    pub id: Uuid,
    pub name: String,
    pub project_id: Uuid,
    pub hypothesis: Option<String>,
    pub status: ExperimentStatus,
    pub host: Option<String>,
    pub git_hash: Option<String>,
    pub config: Option<serde_json::Value>,
    pub metrics: serde_json::Value,
    pub notes: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewExperiment {
    pub name: String,
    pub project_id: Uuid,
    #[serde(default)]
    pub hypothesis: Option<String>,
    #[serde(default)]
    pub status: ExperimentStatus,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub git_hash: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExperimentPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hypothesis: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<ExperimentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_hash: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Option<serde_json::Value>>,
    #[serde(default)]
    pub metrics: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone, Default)]
pub struct ListExperiments {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub project_id: Option<Uuid>,
    pub status: Option<ExperimentStatus>,
    pub tag: Option<String>,
}

// ---------------------------------------------------------------------------
// Cross-entity relations (knowledge / citation graph)
// ---------------------------------------------------------------------------

/// One row of the polymorphic `entity_relations` table. Encodes a
/// directed edge from a source entity to a target entity with a
/// typed `kind` (e.g. `"wiki_link"`, `"cites"`, `"derived_from"`).
/// The primary key `(source_id, source_type, target_id, target_type,
/// relation_kind)` ensures edge identity is unique per kind — a
/// repeated add is a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub source_id: Uuid,
    pub source_type: EntityType,
    pub target_id: Uuid,
    pub target_type: EntityType,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Provenance: DataSource and SyncMetadata
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    Bidirectional,
    ImportOnly,
    ExportOnly,
}

impl SyncDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            SyncDirection::Bidirectional => "bidirectional",
            SyncDirection::ImportOnly => "import_only",
            SyncDirection::ExportOnly => "export_only",
        }
    }

    pub fn from_db(s: &str) -> Result<Self, DataError> {
        Ok(match s {
            "bidirectional" => SyncDirection::Bidirectional,
            "import_only" => SyncDirection::ImportOnly,
            "export_only" => SyncDirection::ExportOnly,
            other => {
                return Err(DataError::InvalidEnum {
                    field: "sync_direction",
                    value: other.to_string(),
                })
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMetadata {
    pub entity_id: Uuid,
    pub entity_type: EntityType,
    pub provider: String,
    pub external_id: String,
    pub last_synced_at: DateTime<Utc>,
    pub sync_direction: SyncDirection,
    pub sync_hash: Option<String>,
}

/// Provenance for a single entity. Stored physically in the `sync_metadata`
/// table; reconstructed in memory by the per-entity ops modules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataSource {
    Native,
    SyncSource(SyncMetadata),
}
