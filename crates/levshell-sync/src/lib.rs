//! Levshell sync engine.
//!
//! Hosts the [`SyncAdapter`] trait implementations that import data from
//! external tools (Obsidian, Zotero, AnkiConnect, CalDAV, ...) into the
//! unified data store. Sync adapters are isolated from the rest of the
//! daemon: they only write to `levshell-data`, and a sync failure must
//! never propagate into the shell.

#![forbid(unsafe_code)]
