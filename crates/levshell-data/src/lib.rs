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
    DataSource, EntityType, Event, EventPatch, Experiment, ExperimentPatch, ExperimentStatus,
    Flashcard, FlashcardPatch, ListEvents, ListExperiments, ListFlashcards, ListNotes,
    ListProjects, ListReferences, ListTasks, NewEvent, NewExperiment, NewFlashcard, NewNote,
    NewProject, NewReference, NewTask, Note, NotePatch, Project, ProjectPatch, ProjectStatus,
    Reference, ReferencePatch, Relation, SyncDirection, SyncMetadata, Task, TaskPatch,
    TaskPriority, TaskStatus,
};
pub use ops::{
    ImportReport, NoteSearchHit, RelatedEntity, RelationDirection, ReferenceSearchHit, RowMap,
    StoreExport, SCAFFOLD_RELATION_KIND, EXPORT_VERSION,
};
pub use store::DataStore;
