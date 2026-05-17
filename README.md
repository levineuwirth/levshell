# Levshell

A context-aware research environment for Sway/Wayland, built for CS/AI students
and researchers. Rust daemon + unified SQLite data model + QuickShell (QML)
rendering layer.

> **Status:** Phases 0–2 complete. The daemon runs ~17 modules and 4 external
> sync adapters end-to-end; the QuickShell layer renders the bar, command
> palette, notification center, clock/calendar hub, quick-settings flyout, and
> the warmup / rubber-duck overlays. Used as a daily-driver shell.

See [`spec/levshell-spec.pdf`](spec/levshell-spec.pdf) for the full design and
[`spec/levshell-design.pdf`](spec/levshell-design.pdf) for the visual system.

## Architecture

Three components cooperate over a typed protocol:

1. **`levshell-daemon`** — the Rust brain. Owns the data store, event bus,
   module system, sync engine, and Sway / IPC integration. No rendering.
2. **Unified data store** — embedded SQLite (WAL mode, FTS5) holding every
   research entity: projects, notes, references, flashcards, events, tasks,
   experiments, polymorphic tags, entity relations, wiki-link graph, sync
   metadata. Canonical source of truth.
3. **QuickShell (QML)** — the rendering layer under `shell/`. Receives
   length-prefixed JSON state patches from the daemon over a Unix domain socket
   and emits user events back. QML never touches the database, never polls,
   never talks to Sway directly.

**Invariant:** everything flows through the unified data model. External tools
(Obsidian, Zotero, Anki, CalDAV) are reached via *sync adapters* in
`levshell-sync`; a sync failure must never propagate into the shell.

## Workspace layout

```
crates/
  levshell-core      Event bus, Module trait, ModuleRunner, health state
  levshell-data      SQLite store, migrations, typed CRUD, FTS5, sync metadata
  levshell-ipc       DaemonMessage/ShellMessage, framing, codec, IpcServer
  levshell-context   Context engine + profile activation
  levshell-config    Config loading + inotify hot-reload (profiles/sync/themes)
  levshell-projects  Project registry (manual attach/detach, runtime metadata)
  levshell-sync      External-tool sync adapters + SyncEngine scheduler
  levshell-modules   Concrete Module impls (telemetry, palette, context, …)
  levshell-daemon    lib + bin — orchestrates everything
  levshell-ctl       One-shot CLI client (ping/status/profile/palette/…)

shell/               QuickShell QML entry point and widget components
config/              Example user config (levshell.toml, profiles, themes, …)
spec/                Design specification + visual design doc (PDF + LaTeX)
```

Dependency direction is strictly layered: `core` is a leaf, `data` / `ipc` sit
above it, `modules` / `sync` / `daemon` sit on top. Adding a reverse edge is a
design smell — raise it before implementing.

## Build & run

Prerequisites: Rust ≥ 1.80, `sqlite` headers are bundled, Sway running under
Wayland, and [`quickshell`](https://quickshell.outfoxxed.me/) installed.
Optional, for full functionality: `brightnessctl` (quick-settings brightness),
a local Ollama endpoint (rubber-duck debugger), and `ssh` (remote-host triad).

```bash
# Build the entire workspace.
cargo build --workspace            # add --release for daily-driver use

# Full test suite (no Sway / network required).
cargo test --workspace

# Lint gate (must stay silent).
cargo clippy --workspace --all-targets -- -D warnings

# Run the daemon in the foreground with tracing enabled.
RUST_LOG=levshell=debug cargo run -p levshell-daemon

# In a second terminal, launch the QML shell.
quickshell -p shell/main.qml

# Drive the running daemon.
cargo run -p levshell-ctl -- status
```

The daemon binds `$XDG_RUNTIME_DIR/levshell.sock` on startup and removes a
stale socket on the next bind. The SQLite database lives at
`$XDG_DATA_HOME/levshell/levshell.db` (falling back to
`~/.local/share/levshell/`). User config is read from
`~/.config/levshell/` — copy the example `config/` tree there to start.

**Note:** the daemon uses a single-shell session model and shuts down when its
attached shell disconnects, so a `quickshell` restart requires a daemon
restart too.

## Phase status

- **Phase 0 — Foundation.** Migrations + async CRUD, typed event bus with
  slow-consumer drop, `Module` trait + `ModuleRunner` with `HealthState`
  (Normal / Stale / Error / Unavailable), length-prefixed JSON IPC, and the
  `SwayWorkspaceModule` vertical slice.
- **Phase 1 — Shell & context.** Context engine + TOML profile loader,
  telemetry modules (cpu / memory / battery / network), command palette with
  app-launcher recency + workspace + note-search providers, the full
  QuickShell visual layer (density morphing, Phosphor icons, per-theme icon
  resolver, TOML theme loader), notification center, and the urgency /
  escalation grammar with a freedesktop notification bridge for critical
  escalations.
- **Phase 2 — Data & integration.** Complete CRUD for every unified-data-model
  entity, the `SyncEngine` scheduler with Obsidian / Zotero / AnkiConnect /
  CalDAV adapters (hot-reloaded config, isolated failure), project registry,
  the SSH / GPU / remote-jobs monitor triad, ideation engine, warmup mode,
  interruption-cost awareness, named sway-tree context snapshots, the
  lit-review / writing auto-activating profiles, and the local-LLM
  rubber-duck debugger overlay.

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
- A sync adapter failure is logged and isolated — it never reaches the shell
  or aborts other adapters.
- QML widgets opt into click/hover via `WidgetWrapper`'s `interactive` flag and
  `clicked()` signal; they must not inject a full-bleed `MouseArea` into the
  measured content slot (it closes a `targetWidth` binding loop).

## License

MIT — see [`LICENSE`](LICENSE).
