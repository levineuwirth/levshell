//! Levshell unified data model.
//!
//! Owns the SQLite schema, embedded migrations, query API, and FTS5 search.
//! All entity types (Project, Note, Reference, Flashcard, Event, Task,
//! Experiment) and the polymorphic tag/relation tables live here. Modules
//! query this crate through a typed API; raw SQL never leaves the crate
//! boundary.

#![forbid(unsafe_code)]

mod error;
mod models;
mod ops;
mod store;

pub use error::{DataError, Result};
pub use models::{
    DataSource, EntityType, Event, Experiment, ExperimentStatus, Flashcard, ListNotes,
    ListProjects, NewNote, NewProject, NewReference, Note, NotePatch, Project, ProjectPatch,
    ProjectStatus, Reference, SyncDirection, SyncMetadata, Task, TaskPriority, TaskStatus,
};
pub use ops::{NoteSearchHit, ReferenceSearchHit};
pub use store::DataStore;
