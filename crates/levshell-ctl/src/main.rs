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
        CtlResponse::Error { message } => {
            eprintln!("error: {message}");
        }
        // `CtlResponse` is `#[non_exhaustive]`; future variants print a
        // placeholder so old ctl binaries stay usable against newer daemons.
        _ => println!("(unknown response from daemon)"),
    }
}
