//! Levshell project registry (§3.7).
//!
//! A **project** is the fundamental unit of research context — it links
//! notes, references, flashcards, experiments, tasks, and events into a
//! coherent research thread. Projects can be created natively inside
//! Levshell via the unified data store, or declared in TOML files under
//! `~/.config/levshell/projects/*.toml`. The registry is the single
//! source of truth that merges the two: it upserts TOML-defined projects
//! into the DB on startup, then indexes them in-memory alongside metadata
//! that doesn't belong in the data model (git repos, SSH hosts, workspace
//! names, accent color, matching tags).
//!
//! Modules query the registry to answer questions like "which project is
//! associated with the active workspace?" or "which project should I
//! auto-attach this imported Obsidian note to?". Users can also
//! explicitly attach/detach entities via `levshell-ctl attach`.
//!
//! The registry is **not** a [`levshell_core::Module`] — it has no widget,
//! no ticks, no event subscription. It is a shared service held behind
//! `Arc` and passed to the daemon's ctl dispatcher.

#![forbid(unsafe_code)]

pub mod config;
pub mod registry;

pub use config::{
    default_projects_dir, load_project_file, load_projects_from_dir, ProjectFile,
    ProjectRegistryConfigError,
};
pub use registry::{
    ProjectEntry, ProjectMetadata, ProjectRegistry, ProjectRegistryError, ProjectRegistryWatcher,
};
