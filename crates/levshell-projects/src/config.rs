//! TOML schema and loader for project configuration files.
//!
//! A project file looks like:
//!
//! ```toml
//! # ~/.config/levshell/projects/llm-alignment.toml
//! name = "LLM Alignment"
//! status = "active"   # optional: active | simmering | blocked | writing_up | complete
//! description = "Research on alignment of large language models"
//! open_questions = ["How to measure deception?", "Which metrics transfer?"]
//! tags = ["llm", "alignment", "research"]
//!
//! git_repos = ["/home/user/code/alignment"]
//! ssh_hosts = ["gpu-cluster-3"]
//! workspace_names = ["research-llm"]
//! accent_color = "#7aa2f7"
//! ```
//!
//! Fields above the blank line map 1:1 into the [`projects`] table;
//! fields below it are *runtime metadata* held in-memory by the
//! [`crate::ProjectRegistry`]. Missing `name` is a parse error (the
//! registry keys by name for upsert). Missing `status` defaults to
//! `active`. Everything else defaults to empty / none.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use levshell_data::ProjectStatus;

/// Errors the project config loader can emit. Parse errors are returned
/// here but [`load_projects_from_dir`] logs and skips malformed files
/// rather than propagating — the daemon should always boot.
#[derive(Debug, Error)]
pub enum ProjectRegistryConfigError {
    #[error("reading project file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing project file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("project file {path} is missing required field: name")]
    MissingName { path: PathBuf },
}

/// Deserialized form of a single project TOML file. Converts to a
/// `(NewProject, ProjectMetadata)` pair via [`Self::into_project_and_metadata`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFile {
    pub name: String,
    #[serde(default)]
    pub status: Option<ProjectStatus>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,

    /// Tags used by the registry's auto-attach heuristic: an entity
    /// tagged with any of these will be associated with this project.
    #[serde(default)]
    pub tags: Vec<String>,

    /// Filesystem paths to Git repositories associated with the project.
    /// Used by the Git/CI modules (Phase 2+) to attribute repo state to
    /// the right project.
    #[serde(default)]
    pub git_repos: Vec<PathBuf>,

    /// Remote hosts the user SSHs into for this project's work. The
    /// SSH-monitor module (Phase 2+) uses these to label active sessions.
    #[serde(default)]
    pub ssh_hosts: Vec<String>,

    /// Workspace names (Sway workspaces) that should be treated as
    /// "belonging to" this project. Feeds the context engine's
    /// workspace → project mapping.
    #[serde(default)]
    pub workspace_names: Vec<String>,

    /// Optional accent color override (hex string like "#7aa2f7"). When
    /// the project is active, the shell's accent shifts to this color.
    #[serde(default)]
    pub accent_color: Option<String>,
}

/// Load a single project file and return the parsed shape. Rejected with
/// [`ProjectRegistryConfigError::MissingName`] if the TOML parses but
/// leaves `name` empty.
pub fn load_project_file(path: &Path) -> Result<ProjectFile, ProjectRegistryConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ProjectRegistryConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let file: ProjectFile =
        toml::from_str(&text).map_err(|e| ProjectRegistryConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;
    if file.name.trim().is_empty() {
        return Err(ProjectRegistryConfigError::MissingName {
            path: path.to_path_buf(),
        });
    }
    Ok(file)
}

/// Scan a directory for `*.toml` files and load each as a project.
/// Malformed files log a warning and are skipped; the returned vector is
/// sorted by project name for determinism.
pub fn load_projects_from_dir(dir: &Path) -> Vec<ProjectFile> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "levshell-projects: failed to read projects directory"
                );
            }
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_project_file(&path) {
            Ok(file) => out.push(file),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "levshell-projects: skipping malformed project file"
                );
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Default projects directory: `$XDG_CONFIG_HOME/levshell/projects` or
/// `~/.config/levshell/projects`. Returns `None` if neither env var is set.
pub fn default_projects_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("levshell").join("projects"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn parses_full_project_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "llm.toml",
            r##"
name = "LLM Alignment"
status = "active"
description = "LLM alignment research"
open_questions = ["How to measure deception?"]
tags = ["llm", "alignment"]
git_repos = ["/home/user/code/alignment"]
ssh_hosts = ["gpu-cluster-3"]
workspace_names = ["research-llm"]
accent_color = "#7aa2f7"
"##,
        );
        let file = load_project_file(&dir.path().join("llm.toml")).unwrap();
        assert_eq!(file.name, "LLM Alignment");
        assert_eq!(file.status, Some(ProjectStatus::Active));
        assert_eq!(file.open_questions.len(), 1);
        assert_eq!(file.tags, vec!["llm", "alignment"]);
        assert_eq!(file.ssh_hosts, vec!["gpu-cluster-3"]);
        assert_eq!(file.accent_color.as_deref(), Some("#7aa2f7"));
    }

    #[test]
    fn minimal_file_has_sensible_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "minimal.toml", r#"name = "Quick Project""#);
        let file = load_project_file(&dir.path().join("minimal.toml")).unwrap();
        assert_eq!(file.name, "Quick Project");
        assert!(file.tags.is_empty());
        assert!(file.git_repos.is_empty());
        assert!(file.status.is_none(), "status is Option — registry fills default");
    }

    #[test]
    fn absent_name_field_is_a_parse_error() {
        // `name` is required by the serde schema (not `Option`), so an
        // absent field fails at TOML deserialization. The dedicated
        // `MissingName` variant kicks in for the *present but blank*
        // case — exercised by `empty_name_rejected` below.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "no-name.toml", r#"description = "anon""#);
        let err = load_project_file(&dir.path().join("no-name.toml")).unwrap_err();
        assert!(matches!(err, ProjectRegistryConfigError::Parse { .. }));
    }

    #[test]
    fn empty_name_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "blank.toml", "name = \"   \"");
        let err = load_project_file(&dir.path().join("blank.toml")).unwrap_err();
        assert!(matches!(err, ProjectRegistryConfigError::MissingName { .. }));
    }

    #[test]
    fn load_dir_sorts_and_skips_malformed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "zulu.toml", r#"name = "Zulu""#);
        write(dir.path(), "alpha.toml", r#"name = "Alpha""#);
        write(dir.path(), "broken.toml", "name = = = malformed");
        write(dir.path(), "README.md", "not a project");

        let files = load_projects_from_dir(dir.path());
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "Alpha");
        assert_eq!(files[1].name, "Zulu");
    }

    #[test]
    fn load_dir_returns_empty_when_directory_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(load_projects_from_dir(&missing).is_empty());
    }
}
