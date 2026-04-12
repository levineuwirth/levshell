//! Per-entity operations on [`DataStore`].
//!
//! Each submodule adds an `impl DataStore` block with the methods for one
//! entity type. The split keeps each file focused on a single SQL table while
//! the public API on `DataStore` remains flat: callers see
//! `store.insert_project(...)` and `store.search_notes(...)` without having to
//! navigate sub-stores.
//!
//! [`DataStore`]: crate::store::DataStore

mod notes;
mod projects;
mod refs;
mod search;
mod sync_metadata;
mod tags;

pub use search::{NoteSearchHit, ReferenceSearchHit};
