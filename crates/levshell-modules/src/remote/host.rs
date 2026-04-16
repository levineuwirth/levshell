//! Named-host registry (spec §2.5.2 "Named Host Profiles").
//!
//! Hosts are declared in `~/.config/levshell/hosts/*.toml`. Each file
//! contains one or more `[[host]]` blocks. A `HostConfig` names the
//! host (`lab-gpu`), carries the real DNS address and SSH user, an
//! optional display name used by widgets, and a list of *roles* that
//! control which modules observe the host:
//!
//! - `ssh_monitor` — include in the SSH Connection Dashboard
//! - `gpu` — include in the GPU Utilization Dashboard
//! - `jobs` — include in the Remote Job Monitor (SLURM)
//!
//! A host with no roles is registered but dormant — useful for keeping
//! a declaration around when the user temporarily loses access.
//!
//! The registry is a read-only snapshot. Hot-reloading host TOML
//! changes is future work; for v1 the daemon reads the directory once
//! at startup and uses that snapshot for the session.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Wire form of `~/.config/levshell/hosts/*.toml`. One file may
/// declare multiple hosts via `[[host]]` arrays.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostFile {
    #[serde(default, rename = "host")]
    pub hosts: Vec<HostConfig>,
}

/// A single named host. `name` is the canonical identifier used by
/// modules and logs; `hostname` is what gets handed to `ssh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    /// Stable identifier, e.g. `"lab-gpu"`. Used as the key inside
    /// module state and as the log field.
    pub name: String,

    /// DNS name or IP handed to `ssh`. Use the special value
    /// `"localhost"` to run commands directly without ssh (useful for
    /// monitoring the user's own workstation with nvidia-smi).
    pub hostname: String,

    /// SSH user. Falls back to the current OS user when omitted —
    /// matches the `ssh` default.
    #[serde(default)]
    pub user: Option<String>,

    /// Optional SSH port. Defaults to 22 (implicit in ssh).
    #[serde(default)]
    pub port: Option<u16>,

    /// Human-readable name for bar widgets ("Lab GPU Cluster").
    /// Defaults to [`Self::name`].
    #[serde(default)]
    pub display_name: Option<String>,

    /// Project this host is associated with. Currently advisory — the
    /// ideation engine and context engine may use it later to color
    /// widgets.
    #[serde(default)]
    pub project: Option<String>,

    /// Poll interval override. Each module has a sensible default;
    /// this overrides them per-host.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,

    /// SSH connect timeout. Defaults to 5 seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,

    /// Which modules observe this host. Empty means dormant.
    #[serde(default)]
    pub roles: Vec<HostRole>,
}

fn default_connect_timeout_secs() -> u64 {
    5
}

/// Which Levshell module cares about this host.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRole {
    /// The SSH Connection Dashboard tracks reachability + latency.
    SshMonitor,
    /// The GPU Utilization Dashboard polls `nvidia-smi`.
    Gpu,
    /// The Remote Job Monitor polls SLURM `squeue`.
    Jobs,
}

impl HostConfig {
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs.max(1))
    }

    pub fn has_role(&self, role: HostRole) -> bool {
        self.roles.contains(&role)
    }

    /// Whether this "host" is actually the local machine. Transport
    /// short-circuits ssh in that case.
    pub fn is_local(&self) -> bool {
        matches!(self.hostname.as_str(), "localhost" | "127.0.0.1" | "::1")
    }
}

#[derive(Debug, Error)]
pub enum HostRegistryError {
    #[error("reading host config dir {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("reading host config file {path}: {source}")]
    FileIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing host config {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("duplicate host name {name} in {path}")]
    DuplicateName { name: String, path: PathBuf },
}

/// A session-scoped immutable registry. Wrapped in `Arc` wherever it
/// crosses a module boundary; `HostConfig` fields are small enough to
/// clone cheaply.
#[derive(Debug, Clone, Default)]
pub struct HostRegistry {
    hosts: Vec<HostConfig>,
}

impl HostRegistry {
    pub fn new(hosts: Vec<HostConfig>) -> Self {
        Self { hosts }
    }

    /// Load every `*.toml` in `dir`. Missing dir → empty registry;
    /// parse failures in individual files are logged and skipped so
    /// one broken file doesn't kill the daemon. Duplicate names
    /// across files *do* fail — they'd make module state ambiguous.
    pub fn load_from_dir(dir: &Path) -> Result<Self, HostRegistryError> {
        if !dir.exists() {
            return Ok(Self::default());
        }
        let entries = std::fs::read_dir(dir).map_err(|source| HostRegistryError::Io {
            path: dir.to_path_buf(),
            source,
        })?;

        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("toml"))
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        paths.sort();

        let mut hosts: Vec<HostConfig> = Vec::new();
        for path in paths {
            match load_file(&path) {
                Ok(mut file_hosts) => {
                    for h in file_hosts.drain(..) {
                        if hosts.iter().any(|existing| existing.name == h.name) {
                            return Err(HostRegistryError::DuplicateName {
                                name: h.name,
                                path,
                            });
                        }
                        hosts.push(h);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping malformed host config"
                    );
                }
            }
        }
        Ok(Self { hosts })
    }

    pub fn hosts(&self) -> &[HostConfig] {
        &self.hosts
    }

    pub fn by_name(&self, name: &str) -> Option<&HostConfig> {
        self.hosts.iter().find(|h| h.name == name)
    }

    pub fn filter_by_role(&self, role: HostRole) -> Vec<&HostConfig> {
        self.hosts.iter().filter(|h| h.has_role(role)).collect()
    }
}

fn load_file(path: &Path) -> Result<Vec<HostConfig>, HostRegistryError> {
    let text = std::fs::read_to_string(path).map_err(|source| HostRegistryError::FileIo {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: HostFile = toml::from_str(&text).map_err(|source| HostRegistryError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parsed.hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn empty_dir_yields_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let reg = HostRegistry::load_from_dir(dir.path()).unwrap();
        assert!(reg.hosts().is_empty());
    }

    #[test]
    fn missing_dir_yields_empty_registry() {
        let reg = HostRegistry::load_from_dir(Path::new("/nope/nope")).unwrap();
        assert!(reg.hosts().is_empty());
    }

    #[test]
    fn parses_multiple_hosts_across_files() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.toml",
            r#"
[[host]]
name = "lab-gpu"
hostname = "gpu.lab.edu"
user = "l"
display_name = "Lab GPU Cluster"
roles = ["ssh_monitor", "gpu"]
"#,
        );
        write(
            dir.path(),
            "b.toml",
            r#"
[[host]]
name = "hpc"
hostname = "hpc.edu"
roles = ["ssh_monitor", "jobs"]
"#,
        );
        let reg = HostRegistry::load_from_dir(dir.path()).unwrap();
        assert_eq!(reg.hosts().len(), 2);

        let gpu = reg.filter_by_role(HostRole::Gpu);
        assert_eq!(gpu.len(), 1);
        assert_eq!(gpu[0].name, "lab-gpu");

        let ssh = reg.filter_by_role(HostRole::SshMonitor);
        assert_eq!(ssh.len(), 2);

        let jobs = reg.filter_by_role(HostRole::Jobs);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "hpc");
    }

    #[test]
    fn duplicate_name_across_files_errors() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.toml",
            r#"
[[host]]
name = "dup"
hostname = "a"
"#,
        );
        write(
            dir.path(),
            "b.toml",
            r#"
[[host]]
name = "dup"
hostname = "b"
"#,
        );
        let err = HostRegistry::load_from_dir(dir.path()).unwrap_err();
        assert!(matches!(err, HostRegistryError::DuplicateName { .. }));
    }

    #[test]
    fn malformed_file_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "broken.toml", "this is = not { valid toml");
        write(
            dir.path(),
            "good.toml",
            r#"
[[host]]
name = "real"
hostname = "real.example"
"#,
        );
        let reg = HostRegistry::load_from_dir(dir.path()).unwrap();
        assert_eq!(reg.hosts().len(), 1);
        assert_eq!(reg.hosts()[0].name, "real");
    }

    #[test]
    fn defaults_are_sensible() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "h.toml",
            r#"
[[host]]
name = "bare"
hostname = "bare.example"
"#,
        );
        let reg = HostRegistry::load_from_dir(dir.path()).unwrap();
        let h = &reg.hosts()[0];
        assert_eq!(h.display_name(), "bare");
        assert_eq!(h.connect_timeout_secs, 5);
        assert!(h.roles.is_empty(), "dormant by default");
        assert!(!h.is_local());
        assert!(h.user.is_none());
    }

    #[test]
    fn localhost_host_is_local() {
        let h = HostConfig {
            name: "local".into(),
            hostname: "localhost".into(),
            user: None,
            port: None,
            display_name: None,
            project: None,
            poll_interval_secs: None,
            connect_timeout_secs: 5,
            roles: vec![HostRole::Gpu],
        };
        assert!(h.is_local());
    }
}
