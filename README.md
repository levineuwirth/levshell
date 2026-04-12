# Levshell

A context-aware research environment for Sway/Wayland, built for CS/AI students
and researchers. Rust daemon + unified SQLite data model + QuickShell (QML)
rendering layer.

> **Status:** Phase 0 (Foundation) — complete. Workspace indicator vertical slice
> runs end-to-end. Phases 1+ (context engine, telemetry, notifications, command
> palette, widgets) are not yet implemented.

See [`spec/levshell-spec.pdf`](spec/levshell-spec.pdf) for the full design.

## Architecture

Three components cooperate over a typed protocol:

1. **`levshell-daemon`** — the Rust brain. Owns the data store, event bus,
   module system, and Sway / IPC integration. No rendering.
2. **Unified data store** — embedded SQLite (WAL mode, FTS5) holding every
   research entity: projects, notes, references, flashcards, events, tasks,
   experiments, polymorphic tags, entity relations, sync metadata. Canonical
   source of truth.
3. **QuickShell (QML)** — the rendering layer under `shell/`. Receives
   length-prefixed JSON state patches from the daemon over a Unix domain socket
   and emits user events back. QML never touches the database, never polls,
   never talks to Sway directly.

**Invariant:** everything flows through the unified data model. External tools
(Obsidian, Zotero, Anki, …) will be reached via *sync adapters* in
`levshell-sync`; a sync failure must never propagate into the shell.

## Workspace layout

```
crates/
  levshell-core      Event bus, Module trait, ModuleRunner, health state
  levshell-data      SQLite store, migrations, typed CRUD, FTS5, sync metadata
  levshell-ipc       DaemonMessage/ShellMessage, framing, codec, IpcServer
  levshell-context   (Phase 1) context engine
  levshell-sync      (Phase 2) external-tool sync adapters
  levshell-config    (Phase 1) config loading / hot reload
  levshell-modules   Concrete Module impls (Phase 0: SwayWorkspaceModule)
  levshell-daemon    lib + bin — orchestrates everything

shell/               QuickShell QML entry point and widget components
config/              Example user config (levshell.toml, profiles, themes, …)
spec/                Design specification (PDF + LaTeX source)
```

Dependency direction is strictly layered: `core` is a leaf, `data` / `ipc` sit
above it, `modules` and `daemon` sit on top. Adding a reverse edge is a design
smell — raise it before implementing.

## Build & run

Prerequisites: Rust ≥ 1.80, `sqlite` headers are bundled, Sway running under
Wayland, and [`quickshell`](https://quickshell.outfoxxed.me/) installed.

```bash
# Build the entire workspace.
cargo build --workspace

# Full test suite (35 tests across 4 crates; no Sway required).
cargo test --workspace

# Lint gate (must stay silent).
cargo clippy --workspace --all-targets -- -D warnings

# Run the daemon in the foreground with tracing enabled.
RUST_LOG=levshell=debug cargo run -p levshell-daemon

# In a second terminal, launch the QML shell.
quickshell -p shell/main.qml
```

The daemon binds `$XDG_RUNTIME_DIR/levshell.sock` on startup and unlinks it on
clean shutdown. The SQLite database lives at
`$XDG_DATA_HOME/levshell/data.db` (falling back to `~/.local/share/levshell/`).

## Phase 0 scope

What the foundation gives you:

- **Data**: migrations, async CRUD for projects + notes, polymorphic tags, FTS5
  search over notes/refs, sync-metadata provenance.
- **Core**: typed event bus with slow-consumer drop, `Module` async trait,
  `ModuleRunner` with `HealthState` (Normal / Stale / Error / Unavailable) and
  `2× tick_interval` staleness timeout.
- **IPC**: length-prefixed (4-byte BE u32, 16 MB cap) JSON frames over a Unix
  socket, splittable `IpcConnection`, `WidgetPublisher` + writer task.
- **Vertical slice**: `SwayWorkspaceModule` subscribes to `swayipc-async`,
  publishes `WorkspaceChanged` / `WindowFocused` to the bus, and pushes
  `WidgetUpdate` frames that the QML bar renders as workspace pills + focused
  window title.

What is **not** yet implemented: context engine, telemetry modules,
notifications, command palette, clock / calendar / quick-settings widgets,
config loading, sync adapters, command-palette backends.

## Project conventions

- SQLite is intentionally synchronous; every DB call goes through
  `spawn_blocking` inside `DataStore::with_conn`.
- Entity IDs are UUID v7 (time-ordered), stored as `BLOB(16)`.
- IPC messages use `#[serde(tag = "type")]` — the `type` discriminator is part
  of the wire contract; do not rename variants casually.
- Modules never touch shared state directly. All communication goes through
  the event bus or the `WidgetPublisher` channel.
- A module that cannot reasonably start (e.g. Sway not running) returns
  `ModuleError::Unavailable` from `start()`; the runner parks it rather than
  crashing the daemon.

## License

MIT — see [`LICENSE`](LICENSE).
