//! `levshell-ctl` — one-shot CLI client for the Levshell daemon.
//!
//! Connects to `$XDG_RUNTIME_DIR/levshell.sock` (or `--socket`), sends a
//! [`Hello`] handshake declaring itself as [`ClientRole::Ctl`], writes a
//! single [`CtlRequest`], reads a single [`CtlResponse`], prints the result,
//! and exits. Designed to be called from Sway keybinds, shell scripts, or
//! cron — not long-lived.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use levshell_ipc::{
    default_socket_path, BarDensity, ClientRole, CtlRequest, CtlResponse, Hello, IpcConnection,
    JsonCodec, PaletteAction, ProfileAction,
};
use tokio::net::UnixStream;

/// Command-line control interface for the Levshell daemon.
#[derive(Debug, Parser)]
#[command(name = "levshell-ctl", version, about)]
struct Cli {
    /// Override the socket path. Defaults to `$XDG_RUNTIME_DIR/levshell.sock`.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Round-trip liveness check. Prints `pong` if the daemon replies.
    Ping,

    /// Print a health snapshot of the running daemon.
    Status,

    /// Request a bar-density change.
    Density {
        #[arg(value_enum)]
        mode: CliDensity,
    },

    /// Activate, cycle, or query a context profile.
    Profile {
        #[command(subcommand)]
        action: ProfileCmd,
    },

    /// Open, close, toggle, or query the command palette.
    Palette {
        #[command(subcommand)]
        action: PaletteCmd,
    },

    /// List registered projects (from `~/.config/levshell/projects/`).
    Projects,

    /// Attach an entity to a project. Entity types:
    /// `note`, `ref`, `flashcard`, `event`, `task`. The `project`
    /// argument is either the project name or its UUID.
    Attach {
        #[arg(value_enum)]
        entity_type: CliEntityType,
        entity_id: String,
        project: String,
    },

    /// Detach an entity from its current project. Experiments cannot
    /// be detached (their `project_id` is required).
    Detach {
        #[arg(value_enum)]
        entity_type: CliEntityType,
        entity_id: String,
    },
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum CliEntityType {
    Note,
    Ref,
    Flashcard,
    Event,
    Task,
}

impl CliEntityType {
    fn as_wire(self) -> &'static str {
        match self {
            CliEntityType::Note => "note",
            CliEntityType::Ref => "ref",
            CliEntityType::Flashcard => "flashcard",
            CliEntityType::Event => "event",
            CliEntityType::Task => "task",
        }
    }
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum CliDensity {
    Full,
    Compact,
    Hidden,
}

impl From<CliDensity> for BarDensity {
    fn from(value: CliDensity) -> Self {
        match value {
            CliDensity::Full => BarDensity::Full,
            CliDensity::Compact => BarDensity::Compact,
            CliDensity::Hidden => BarDensity::Hidden,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ProfileCmd {
    /// Activate a named profile.
    Activate {
        /// Profile name.
        name: String,
    },
    /// Cycle to the next profile in user order.
    Cycle,
    /// Query the active profile.
    Query,
}

#[derive(Debug, Subcommand)]
enum PaletteCmd {
    Open,
    Close,
    Toggle,
    /// Run a search query against the palette without opening the UI.
    Query {
        /// Search text.
        query: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("levshell-ctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn real_main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = match cli.socket {
        Some(p) => p,
        None => default_socket_path().context("resolving default socket path")?,
    };

    let request = build_request(cli.command);

    let stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("connecting to daemon socket {}", socket_path.display()))?;
    let mut conn = IpcConnection::<JsonCodec>::from_unix_stream(stream);

    conn.writer()
        .send(&Hello::new(ClientRole::Ctl))
        .await
        .context("sending Hello handshake")?;

    conn.writer()
        .send(&request)
        .await
        .context("sending ctl request")?;

    let response: CtlResponse = conn
        .reader()
        .recv()
        .await
        .context("reading ctl response")?;

    print_response(&response);

    match response {
        CtlResponse::Error { .. } => Err(anyhow::anyhow!("daemon returned an error")),
        _ => Ok(()),
    }
}

fn build_request(cmd: Command) -> CtlRequest {
    match cmd {
        Command::Ping => CtlRequest::Ping,
        Command::Status => CtlRequest::Status,
        Command::Density { mode } => CtlRequest::Density { mode: mode.into() },
        Command::Profile { action } => match action {
            ProfileCmd::Activate { name } => CtlRequest::Profile {
                action: ProfileAction::Activate,
                name: Some(name),
            },
            ProfileCmd::Cycle => CtlRequest::Profile {
                action: ProfileAction::Cycle,
                name: None,
            },
            ProfileCmd::Query => CtlRequest::Profile {
                action: ProfileAction::Query,
                name: None,
            },
        },
        Command::Palette { action } => match action {
            PaletteCmd::Open => CtlRequest::Palette {
                action: PaletteAction::Open,
                query: None,
            },
            PaletteCmd::Close => CtlRequest::Palette {
                action: PaletteAction::Close,
                query: None,
            },
            PaletteCmd::Toggle => CtlRequest::Palette {
                action: PaletteAction::Toggle,
                query: None,
            },
            PaletteCmd::Query { query } => CtlRequest::Palette {
                action: PaletteAction::Query,
                query: Some(query),
            },
        },
        Command::Projects => CtlRequest::Projects,
        Command::Attach {
            entity_type,
            entity_id,
            project,
        } => CtlRequest::Attach {
            entity_type: entity_type.as_wire().to_string(),
            entity_id,
            project,
        },
        Command::Detach {
            entity_type,
            entity_id,
        } => CtlRequest::Detach {
            entity_type: entity_type.as_wire().to_string(),
            entity_id,
        },
    }
}

fn print_response(response: &CtlResponse) {
    match response {
        CtlResponse::Ok => println!("ok"),
        CtlResponse::Pong => println!("pong"),
        CtlResponse::Status(s) => {
            println!("protocol_version: {}", s.protocol_version);
            println!("socket_path:      {}", s.socket_path);
            println!("db_path:          {}", s.db_path);
            println!("shell_connected:  {}", s.shell_connected);
            println!("module_count:     {}", s.module_count);
        }
        CtlResponse::Projects { projects } => {
            if projects.is_empty() {
                println!("(no projects registered)");
                return;
            }
            for p in projects {
                let tags = if p.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", p.tags.join(", "))
                };
                let color = p.accent_color.as_deref().unwrap_or("");
                println!("{}  {}  {}{}  {}", p.id, p.status, p.name, tags, color);
                let active = if p.currently_active_workspaces.is_empty() {
                    "—".to_string()
                } else {
                    p.currently_active_workspaces.join(",")
                };
                let last = p.last_active_at.as_deref().unwrap_or("never");
                println!(
                    "    focus: {}s  last_active: {}  active_ws: {}",
                    p.accumulated_focus_time_secs, last, active
                );
            }
        }
        CtlResponse::Error { message } => {
            eprintln!("error: {message}");
        }
        // `CtlResponse` is `#[non_exhaustive]`; future variants print a
        // placeholder so old ctl binaries stay usable against newer daemons.
        _ => println!("(unknown response from daemon)"),
    }
}
