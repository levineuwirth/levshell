//! [`RemoteRunner`] trait — the seam modules call when they need to
//! execute a command on a remote host.
//!
//! Two impls ship in v1:
//!
//! - [`SshRunner`] — spawns `ssh` subprocesses with
//!   `BatchMode=yes`, `ConnectTimeout=N`, and `ControlMaster=auto`
//!   so repeat calls reuse a multiplexed TCP connection. No new
//!   crates — we lean on the system `ssh(1)` the user already
//!   configured.
//! - [`MockRunner`] — test-only. Returns canned output per
//!   `(host_name, argv)` lookup, or a configured error. Lets every
//!   remote module test run offline with deterministic output.
//!
//! A third path is built in: when `host.is_local()` both runners
//! execute the command directly (via `Command::new(argv[0])`) without
//! wrapping it in `ssh`. This is how the GPU dashboard covers the
//! user's own workstation.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

use super::host::HostConfig;

/// What a runner returns on success. Non-zero exit codes are *not*
/// errors — `squeue -h` is a frequent example of a well-behaved
/// command whose exit status of 0 doesn't tell you anything about
/// empty output. Modules inspect `status` themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
}

impl CommandOutput {
    pub fn is_success(&self) -> bool {
        matches!(self.status, Some(0))
    }
}

#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("remote command timed out on host {host}")]
    Timeout { host: String },

    #[error("failed to spawn command on host {host}: {source}")]
    Spawn {
        host: String,
        #[source]
        source: std::io::Error,
    },

    #[error("no mock response configured for host {host} command {cmd:?}")]
    MockMissing { host: String, cmd: Vec<String> },
}

/// Trait that remote-probing modules depend on. Object-safe via
/// `async-trait`.
#[async_trait]
pub trait RemoteRunner: Send + Sync {
    /// Execute `argv` on `host` with a per-call hard timeout. `argv` is
    /// the command + positional args ready for `execve`; runners must
    /// not go through a shell.
    async fn run(
        &self,
        host: &HostConfig,
        argv: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, RemoteError>;
}

// ---------------------------------------------------------------------------
// SshRunner
// ---------------------------------------------------------------------------

/// Production runner. Wraps `tokio::process::Command` with SSH flags
/// that guarantee the subprocess never prompts the user and reuses
/// connections across calls (spec §2.5 — tunnels/probes happen
/// frequently).
#[derive(Debug, Default, Clone)]
pub struct SshRunner {
    /// Optional override for the ssh binary. Default `"ssh"`. Tests
    /// that need to swap in a stub script set this.
    ssh_binary: Option<String>,
}

impl SshRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ssh_binary(binary: impl Into<String>) -> Self {
        Self {
            ssh_binary: Some(binary.into()),
        }
    }

    fn build_command(&self, host: &HostConfig, argv: &[&str]) -> Command {
        if host.is_local() {
            let mut cmd = Command::new(argv[0]);
            if argv.len() > 1 {
                cmd.args(&argv[1..]);
            }
            return cmd;
        }

        let bin = self
            .ssh_binary
            .as_deref()
            .unwrap_or("ssh");
        let mut cmd = Command::new(bin);
        let timeout_arg = format!("ConnectTimeout={}", host.connect_timeout_secs.max(1));
        cmd.args([
            "-o",
            "BatchMode=yes",
            "-o",
            &timeout_arg,
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60",
            "-o",
            // Keep host-key prompts off the subprocess path. Users
            // who want them should run `ssh host` interactively once.
            "StrictHostKeyChecking=accept-new",
        ]);
        if let Some(port) = host.port {
            cmd.arg("-p").arg(port.to_string());
        }
        let target = match &host.user {
            Some(u) => format!("{u}@{}", host.hostname),
            None => host.hostname.clone(),
        };
        cmd.arg(&target);
        cmd.args(argv.iter().map(|s| *s as &str));
        cmd
    }
}

#[async_trait]
impl RemoteRunner for SshRunner {
    async fn run(
        &self,
        host: &HostConfig,
        argv: &[&str],
        wall_timeout: Duration,
    ) -> Result<CommandOutput, RemoteError> {
        if argv.is_empty() {
            return Err(RemoteError::Spawn {
                host: host.name.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "empty argv for remote command",
                ),
            });
        }
        let mut cmd = self.build_command(host, argv);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let fut = async move { cmd.output().await };
        let output = match timeout(wall_timeout, fut).await {
            Ok(r) => r.map_err(|source| RemoteError::Spawn {
                host: host.name.clone(),
                source,
            })?,
            Err(_) => {
                return Err(RemoteError::Timeout {
                    host: host.name.clone(),
                })
            }
        };

        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code(),
        })
    }
}

// ---------------------------------------------------------------------------
// MockRunner
// ---------------------------------------------------------------------------

/// Test-only runner. Register canned (host_name, argv_prefix) →
/// CommandOutput responses; anything unmatched returns
/// `RemoteError::MockMissing`. The `argv_prefix` is matched by
/// string-prefix so tests can register `nvidia-smi` and catch both
/// the query and the version-check invocations.
/// One captured call: (host_name, argv).
pub type MockCall = (String, Vec<String>);

#[derive(Debug, Default, Clone)]
pub struct MockRunner {
    responses: Arc<Mutex<HashMap<(String, String), MockResponse>>>,
    /// Captured calls in arrival order — lets tests assert on
    /// what the module actually issued.
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

#[derive(Debug, Clone)]
enum MockResponse {
    Ok(CommandOutput),
    Timeout,
}

impl MockRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a response for a host+argv-prefix. `argv_prefix` is
    /// matched against the start of the argv: registering
    /// `&["nvidia-smi"]` matches `nvidia-smi --query-gpu=...`.
    pub fn respond(&self, host: &str, argv_prefix: &[&str], output: CommandOutput) -> &Self {
        let key = (host.to_string(), argv_prefix.join(" "));
        self.responses
            .lock()
            .unwrap()
            .insert(key, MockResponse::Ok(output));
        self
    }

    pub fn respond_with_timeout(&self, host: &str, argv_prefix: &[&str]) -> &Self {
        let key = (host.to_string(), argv_prefix.join(" "));
        self.responses
            .lock()
            .unwrap()
            .insert(key, MockResponse::Timeout);
        self
    }

    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    fn lookup(&self, host: &str, argv: &[&str]) -> Option<MockResponse> {
        let guard = self.responses.lock().unwrap();
        for (key, resp) in guard.iter() {
            let (h, prefix) = key;
            if h != host {
                continue;
            }
            let prefix_parts: Vec<&str> = prefix.split_whitespace().collect();
            if argv.len() < prefix_parts.len() {
                continue;
            }
            if argv[..prefix_parts.len()]
                .iter()
                .zip(prefix_parts.iter())
                .all(|(a, b)| *a == *b)
            {
                return Some(resp.clone());
            }
        }
        None
    }
}

#[async_trait]
impl RemoteRunner for MockRunner {
    async fn run(
        &self,
        host: &HostConfig,
        argv: &[&str],
        _timeout: Duration,
    ) -> Result<CommandOutput, RemoteError> {
        self.calls
            .lock()
            .unwrap()
            .push((host.name.clone(), argv.iter().map(|s| s.to_string()).collect()));
        match self.lookup(&host.name, argv) {
            Some(MockResponse::Ok(o)) => Ok(o),
            Some(MockResponse::Timeout) => Err(RemoteError::Timeout {
                host: host.name.clone(),
            }),
            None => Err(RemoteError::MockMissing {
                host: host.name.clone(),
                cmd: argv.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// OS-safe wrapper used by a few module unit tests that need to compare
/// an `OsStr` against a literal.
#[allow(dead_code)]
pub(crate) fn os_eq(a: &OsStr, b: &str) -> bool {
    a.to_str() == Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str) -> HostConfig {
        HostConfig {
            name: name.into(),
            hostname: "host.example".into(),
            user: Some("u".into()),
            port: Some(2222),
            display_name: None,
            project: None,
            poll_interval_secs: None,
            connect_timeout_secs: 3,
            roles: vec![],
        }
    }

    fn local() -> HostConfig {
        HostConfig {
            name: "workstation".into(),
            hostname: "localhost".into(),
            user: None,
            port: None,
            display_name: None,
            project: None,
            poll_interval_secs: None,
            connect_timeout_secs: 5,
            roles: vec![],
        }
    }

    #[test]
    fn ssh_runner_builds_expected_argv() {
        let runner = SshRunner::new();
        let cmd = runner.build_command(&remote("h"), &["nvidia-smi", "--query-gpu=name"]);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // Required flags must all be present, in order, before the
        // target host name.
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ConnectTimeout=3".to_string()));
        assert!(args.contains(&"ControlMaster=auto".to_string()));
        assert!(args.contains(&"ControlPersist=60".to_string()));
        assert!(args.iter().any(|a| a == "u@host.example"));
        assert!(args.iter().any(|a| a == "nvidia-smi"));
        assert!(args.iter().any(|a| a == "--query-gpu=name"));
        // Port forwarded via `-p`.
        assert!(args.contains(&"2222".to_string()));
    }

    #[test]
    fn ssh_runner_local_skips_ssh_wrapping() {
        let runner = SshRunner::new();
        let cmd = runner.build_command(&local(), &["nvidia-smi"]);
        assert_eq!(
            cmd.as_std().get_program().to_string_lossy(),
            "nvidia-smi",
            "local hosts bypass ssh entirely"
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.is_empty(), "no flag baggage for local commands");
    }

    #[tokio::test]
    async fn mock_runner_returns_registered_output() {
        let mock = MockRunner::new();
        mock.respond(
            "h",
            &["true"],
            CommandOutput {
                stdout: "ok\n".into(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let out = mock
            .run(&remote("h"), &["true"], Duration::from_secs(5))
            .await
            .unwrap();
        assert!(out.is_success());
        assert_eq!(out.stdout, "ok\n");
        assert_eq!(mock.calls(), vec![("h".to_string(), vec!["true".to_string()])]);
    }

    #[tokio::test]
    async fn mock_runner_matches_prefix() {
        let mock = MockRunner::new();
        mock.respond(
            "h",
            &["nvidia-smi"],
            CommandOutput {
                stdout: "0, A100, 12, 100, 80000, 45".into(),
                stderr: String::new(),
                status: Some(0),
            },
        );
        let out = mock
            .run(
                &remote("h"),
                &["nvidia-smi", "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu"],
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, "0, A100, 12, 100, 80000, 45");
    }

    #[tokio::test]
    async fn mock_runner_missing_response_errors() {
        let mock = MockRunner::new();
        let err = mock
            .run(&remote("h"), &["unregistered"], Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, RemoteError::MockMissing { .. }));
    }

    #[tokio::test]
    async fn mock_runner_honors_timeout_response() {
        let mock = MockRunner::new();
        mock.respond_with_timeout("h", &["hang"]);
        let err = mock
            .run(&remote("h"), &["hang"], Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, RemoteError::Timeout { .. }));
    }
}
