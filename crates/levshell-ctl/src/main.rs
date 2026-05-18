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
    default_socket_path, BarDensity, ClientRole, ContextSnapshotAction, CtlRequest, CtlResponse,
    DuckAction, Hello, IpcConnection, JsonCodec, NotifyUrgency, PaletteAction, ProfileAction,
    ThemeAction, TimerAction, WarmupAction,
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

    /// Request a bar-density change. `cycle` advances
    /// full -> compact -> hidden -> full server-side.
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

    /// Activate a theme, toggle between paired light/dark variants,
    /// query the active theme, or list available themes. Theme files
    /// live in `~/.config/levshell/themes/<name>.toml`.
    Theme {
        #[command(subcommand)]
        action: ThemeCmd,
    },

    /// Force-fire the warmup overlay (spec §2.12.1). Bypasses the
    /// activity-gap heuristic so you can see the panel without
    /// waiting hours.
    Warmup {
        #[command(subcommand)]
        action: WarmupCmd,
    },

    /// Save / restore / list / delete named context snapshots
    /// (spec §2.12.2). A snapshot captures the current sway window
    /// tree + per-window cmdline; restore moves existing windows back
    /// and re-launches any missing apps.
    Context {
        #[command(subcommand)]
        action: ContextCmd,
    },

    /// Open / close / reset the rubber-duck debugger overlay
    /// (spec §2.12.6). A minimal chat interface to a local LLM for
    /// articulating stuck points.
    Duck {
        #[command(subcommand)]
        action: DuckCmd,
    },

    /// Forward a generic action to a bar widget (spec §2.19.1), e.g.
    /// `levshell-ctl widget ssh-dashboard reconnect host=gpu-3`.
    /// Trailing `key=value` params become a JSON payload the widget's
    /// module receives.
    Widget {
        /// Target widget id (e.g. `ssh-dashboard`).
        widget_id: String,
        /// Action name (e.g. `reconnect`).
        action: String,
        /// Zero or more `key=value` params.
        #[arg(value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Anki / spaced-repetition queries (spec §2.19.1).
    Anki {
        #[command(subcommand)]
        action: AnkiCmd,
    },

    /// Drive the Pomodoro / focus-session timer (spec §2.2.1).
    Timer {
        #[command(subcommand)]
        action: TimerCmd,
    },

    /// Emit a desktop notification (spec §2.19.1), e.g.
    /// `levshell-ctl notify "Build finished" --urgency normal`.
    Notify {
        /// Notification body text.
        body: String,
        /// Summary line. Defaults to "Levshell".
        #[arg(long, default_value = "Levshell")]
        title: String,
        /// Urgency level.
        #[arg(long, value_enum, default_value_t = CliUrgency::Normal)]
        urgency: CliUrgency,
    },
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum CliUrgency {
    Low,
    Normal,
    Critical,
}

impl From<CliUrgency> for NotifyUrgency {
    fn from(value: CliUrgency) -> Self {
        match value {
            CliUrgency::Low => NotifyUrgency::Low,
            CliUrgency::Normal => NotifyUrgency::Normal,
            CliUrgency::Critical => NotifyUrgency::Critical,
        }
    }
}

/// Build a flat JSON object string from `key=value` params. Keys/values
/// are JSON-string-escaped; a param with no `=` maps to an empty string.
/// ctl stays serde_json-free by design, so this is a tiny hand-roller —
/// the daemon validates the result and rejects anything malformed.
fn params_to_json(params: &[String]) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
        out
    }
    let mut s = String::from("{");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let (k, v) = p.split_once('=').unwrap_or((p.as_str(), ""));
        s.push('"');
        s.push_str(&esc(k));
        s.push_str("\":\"");
        s.push_str(&esc(v));
        s.push('"');
    }
    s.push('}');
    s
}

#[derive(Debug, Subcommand)]
enum AnkiCmd {
    /// Print the number of flashcards currently due.
    DueCount,
}

#[derive(Debug, Subcommand)]
enum TimerCmd {
    /// Start a work interval (or resume if paused).
    Start,
    /// Freeze the elapsed counter.
    Pause,
    /// Unfreeze a paused timer.
    Resume,
    /// End the current interval and return to idle.
    Stop,
    /// End the current interval immediately and advance to the next.
    Skip,
}

#[derive(Debug, Subcommand)]
enum DuckCmd {
    /// Reveal the overlay.
    Open,
    /// Hide the overlay without clearing the conversation.
    Close,
    /// Clear the conversation and close the overlay.
    Reset,
}

#[derive(Debug, Subcommand)]
enum ContextCmd {
    /// Capture the current sway tree into `<name>`.
    Save {
        /// Snapshot name. Ascii-alphanumeric + `-`/`_` only.
        name: String,
    },
    /// Apply the saved snapshot `<name>` — move existing windows,
    /// re-launch missing ones via captured cmdlines.
    Restore {
        /// Snapshot name.
        name: String,
    },
    /// List saved snapshot names.
    List,
    /// Delete the saved snapshot `<name>`.
    Delete {
        /// Snapshot name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum WarmupCmd {
    /// Open the warmup overlay now.
    Open,
}

#[derive(Debug, Subcommand)]
enum ThemeCmd {
    /// Activate a theme by file-stem name (e.g. `warm-dark`).
    Set {
        /// Theme name — the `<name>.toml` stem in the themes dir.
        name: String,
    },
    /// Switch between the current theme and its paired variant.
    /// No-op if the current theme doesn't declare `light_pair` /
    /// `dark_pair`.
    ToggleMode,
    /// Print the active theme's name + variant.
    Query,
    /// Enumerate available themes.
    List,
    /// Install bundled default themes into `~/.config/levshell/themes/`.
    /// Does not require the daemon — it's a local file write. Existing
    /// theme files are skipped unless `--force` is passed.
    Bootstrap {
        /// Overwrite existing theme files.
        #[arg(long)]
        force: bool,
    },
    /// Toggle presentation mode (spec §2.18) — mute non-critical
    /// surfaces (nudges, overlays, normal notifications) for talks /
    /// screen-sharing. With no argument, flips the current state.
    Presentation {
        /// `on`, `off`, or `toggle` (default).
        #[arg(value_parser = ["on", "off", "toggle"], default_value = "toggle")]
        state: String,
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
    /// Advance to the next density server-side (full -> compact -> hidden -> full).
    Cycle,
}

impl From<CliDensity> for BarDensity {
    fn from(value: CliDensity) -> Self {
        match value {
            CliDensity::Full => BarDensity::Full,
            CliDensity::Compact => BarDensity::Compact,
            CliDensity::Hidden => BarDensity::Hidden,
            // `Cycle` is dispatched to CtlRequest::DensityCycle in
            // build_request and never reaches this conversion.
            CliDensity::Cycle => unreachable!("cycle is handled before .into()"),
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

    // Local-only commands short-circuit the IPC roundtrip — they don't
    // need a running daemon. Bootstrap writes bundled themes to disk; the
    // daemon's inotify watch picks them up on the next tick.
    if let Command::Theme {
        action: ThemeCmd::Bootstrap { force },
    } = &cli.command
    {
        return run_theme_bootstrap(*force);
    }

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
        Command::Density { mode } => match mode {
            CliDensity::Cycle => CtlRequest::DensityCycle,
            other => CtlRequest::Density { mode: other.into() },
        },
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
        Command::Theme { action } => match action {
            ThemeCmd::Set { name } => CtlRequest::Theme {
                action: ThemeAction::Set,
                name: Some(name),
            },
            ThemeCmd::ToggleMode => CtlRequest::Theme {
                action: ThemeAction::ToggleMode,
                name: None,
            },
            ThemeCmd::Query => CtlRequest::Theme {
                action: ThemeAction::Query,
                name: None,
            },
            ThemeCmd::List => CtlRequest::Theme {
                action: ThemeAction::List,
                name: None,
            },
            ThemeCmd::Presentation { state } => CtlRequest::Theme {
                action: ThemeAction::Presentation,
                name: Some(state),
            },
            ThemeCmd::Bootstrap { .. } => {
                // Handled in real_main before IPC dispatch.
                unreachable!("ThemeCmd::Bootstrap is local-only")
            }
        },
        Command::Warmup { action } => match action {
            WarmupCmd::Open => CtlRequest::Warmup {
                action: WarmupAction::Open,
            },
        },
        Command::Context { action } => match action {
            ContextCmd::Save { name } => CtlRequest::ContextSnapshot {
                action: ContextSnapshotAction::Save,
                name: Some(name),
            },
            ContextCmd::Restore { name } => CtlRequest::ContextSnapshot {
                action: ContextSnapshotAction::Restore,
                name: Some(name),
            },
            ContextCmd::List => CtlRequest::ContextSnapshot {
                action: ContextSnapshotAction::List,
                name: None,
            },
            ContextCmd::Delete { name } => CtlRequest::ContextSnapshot {
                action: ContextSnapshotAction::Delete,
                name: Some(name),
            },
        },
        Command::Duck { action } => match action {
            DuckCmd::Open => CtlRequest::Duck {
                action: DuckAction::Open,
            },
            DuckCmd::Close => CtlRequest::Duck {
                action: DuckAction::Close,
            },
            DuckCmd::Reset => CtlRequest::Duck {
                action: DuckAction::Reset,
            },
        },
        Command::Widget {
            widget_id,
            action,
            params,
        } => CtlRequest::Widget {
            widget_id,
            action,
            data: params_to_json(&params),
        },
        Command::Notify {
            body,
            title,
            urgency,
        } => CtlRequest::Notify {
            title,
            body,
            urgency: urgency.into(),
        },
        Command::Anki { action } => match action {
            AnkiCmd::DueCount => CtlRequest::AnkiDueCount,
        },
        Command::Timer { action } => CtlRequest::Timer {
            action: match action {
                TimerCmd::Start => TimerAction::Start,
                TimerCmd::Pause => TimerAction::Pause,
                TimerCmd::Resume => TimerAction::Resume,
                TimerCmd::Stop => TimerAction::Stop,
                TimerCmd::Skip => TimerAction::Skip,
            },
        },
    }
}

fn run_theme_bootstrap(force: bool) -> Result<()> {
    let dir = levshell_config::default_themes_dir()
        .context("could not resolve $XDG_CONFIG_HOME/levshell/themes")?;
    let report = levshell_config::bootstrap_themes(&dir, force)
        .with_context(|| format!("writing bundled themes to {}", dir.display()))?;
    println!("themes dir: {}", report.dir.display());
    if !report.written.is_empty() {
        println!("installed:");
        for n in &report.written {
            println!("  {n}");
        }
    }
    if !report.skipped.is_empty() {
        println!("skipped (already exist):");
        for n in &report.skipped {
            println!("  {n}");
        }
        if !force {
            println!("(pass --force to overwrite)");
        }
    }
    if report.written.is_empty() && report.skipped.is_empty() {
        println!("(no bundled themes available)");
    }
    Ok(())
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
        CtlResponse::ActiveTheme(t) => {
            let pair = t
                .light_pair
                .as_deref()
                .or(t.dark_pair.as_deref())
                .map(|n| format!(" (pair: {n})"))
                .unwrap_or_default();
            println!("{}  variant: {}{}", t.name, t.variant, pair);
        }
        CtlResponse::Themes { names } => {
            if names.is_empty() {
                println!("(no themes installed)");
                println!("hint: run `levshell-ctl theme bootstrap` to install defaults");
            } else {
                for n in names {
                    println!("{n}");
                }
            }
        }
        CtlResponse::Error { message } => {
            eprintln!("error: {message}");
        }
        CtlResponse::Count { count } => println!("{count}"),
        CtlResponse::ContextSnapshotResult { summary } => println!("{summary}"),
        CtlResponse::ContextSnapshots { names } => {
            if names.is_empty() {
                println!("(no saved contexts)");
            } else {
                for n in names {
                    println!("{n}");
                }
            }
        }
        // `CtlResponse` is `#[non_exhaustive]`; future variants print a
        // placeholder so old ctl binaries stay usable against newer daemons.
        _ => println!("(unknown response from daemon)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_to_json_builds_escaped_object() {
        assert_eq!(params_to_json(&[]), "{}");
        assert_eq!(
            params_to_json(&["host=gpu-3".into()]),
            r#"{"host":"gpu-3"}"#
        );
        assert_eq!(
            params_to_json(&["a=1".into(), "b=2".into()]),
            r#"{"a":"1","b":"2"}"#
        );
        // Bare key (no `=`) maps to empty string.
        assert_eq!(params_to_json(&["flag".into()]), r#"{"flag":""}"#);
        // Quotes/backslashes are escaped so the daemon's JSON parse holds.
        assert_eq!(
            params_to_json(&[r#"msg=a"b\c"#.into()]),
            r#"{"msg":"a\"b\\c"}"#
        );
        // Only the first `=` splits; later ones stay in the value.
        assert_eq!(
            params_to_json(&["expr=x=y".into()]),
            r#"{"expr":"x=y"}"#
        );
    }

    #[test]
    fn cli_parses_widget_and_notify() {
        use clap::Parser;
        let c = Cli::parse_from([
            "levshell-ctl",
            "widget",
            "ssh-dashboard",
            "reconnect",
            "host=gpu-3",
        ]);
        match c.command {
            Command::Widget {
                widget_id,
                action,
                params,
            } => {
                assert_eq!(widget_id, "ssh-dashboard");
                assert_eq!(action, "reconnect");
                assert_eq!(params, vec!["host=gpu-3".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let c = Cli::parse_from([
            "levshell-ctl",
            "notify",
            "Build finished",
            "--urgency",
            "critical",
        ]);
        match c.command {
            Command::Notify {
                body,
                title,
                urgency,
            } => {
                assert_eq!(body, "Build finished");
                assert_eq!(title, "Levshell");
                assert!(matches!(urgency, CliUrgency::Critical));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
