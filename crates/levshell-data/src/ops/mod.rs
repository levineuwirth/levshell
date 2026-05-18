//! Per-entity operations on [`DataStore`].
//!
//! Each submodule adds an `impl DataStore` block with the methods for one
//! entity type. The split keeps each file focused on a single SQL table while
//! the public API on `DataStore` remains flat: callers see
//! `store.insert_project(...)` and `store.search_notes(...)` without having to
//! navigate sub-stores.
//!
//! [`DataStore`]: crate::store::DataStore

mod events;
mod experiments;
mod export;
mod flashcards;
mod graph;
mod notes;
mod projects;
mod refs;
mod relations;
mod scaffold;
mod search;
mod sync_metadata;
mod tags;
mod tasks;

pub use export::{ImportReport, RowMap, StoreExport, EXPORT_VERSION};

pub use graph::{RelatedEntity, RelationDirection};

pub use scaffold::SCAFFOLD_RELATION_KIND;

pub use search::{NoteSearchHit, ReferenceSearchHit};
