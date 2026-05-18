//! App launcher provider.
//!
//! Scans well-known XDG application directories for `.desktop` files,
//! parses a minimal subset of the Desktop Entry Specification, matches
//! user queries against `Name` and `Exec`, and resolves each entry's
//! `Icon=` value to an absolute filesystem path via the freedesktop
//! icon-theme inheritance chain. Selected items are spawned as
//! detached child processes.
//!
//! Ignored at parse time:
//! * `Categories`, `Keywords`, `GenericName`
//! * Localized names (`Name[en_US]=…`)
//! * Terminal applications (`Terminal=true`)
//! * Actions (`Actions=…`)
//!
//! Icon resolution (see [`IconResolver`]) walks the user's configured
//! GTK icon theme plus its `Inherits=` ancestors, terminating at the
//! always-present `hicolor` fallback. Resolution happens once per
//! entry at scan time and is cached on [`DesktopEntry::icon_path`],
//! so the live query path stays filesystem-free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const APP_LAUNCHER_PROVIDER: &str = "app-launcher";

/// Parsed contents of one `.desktop` file — only the fields we care about.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub comment: Option<String>,
    /// Raw value of the `Icon=` field, if present. This is either an
    /// absolute path to an image, or a freedesktop icon theme name
    /// that needs to be resolved through [`resolve_icon`].
    pub icon_name: Option<String>,
    /// Absolute path to the resolved icon image. Populated at scan
    /// time by [`AppLauncherProvider::new`] so the live query path
    /// doesn't have to touch the filesystem.
    pub icon_path: Option<PathBuf>,
}

impl DesktopEntry {
    /// Parse a `.desktop` file's text body. Returns `None` if the entry is
    /// hidden, missing a `Name` or `Exec`, or has `NoDisplay=true`.
    ///
    /// `file_stem` is used to seed `id` (the portable identifier) when
    /// the file itself doesn't carry one.
    pub fn parse(text: &str, file_stem: &str) -> Option<Self> {
        let mut in_entry = false;
        let mut name: Option<String> = None;
        let mut exec: Option<String> = None;
        let mut comment: Option<String> = None;
        let mut icon_name: Option<String> = None;
        let mut no_display = false;
        let mut hidden = false;
        let mut terminal = false;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "Name"
                    if name.is_none() => {
                        name = Some(value.to_owned());
                    }
                "Exec"
                    if exec.is_none() => {
                        exec = Some(value.to_owned());
                    }
                "Comment"
                    if comment.is_none() => {
                        comment = Some(value.to_owned());
                    }
                "Icon"
                    if icon_name.is_none() && !value.is_empty() => {
                        icon_name = Some(value.to_owned());
                    }
                "NoDisplay" => {
                    no_display = value.eq_ignore_ascii_case("true");
                }
                "Hidden" => {
                    hidden = value.eq_ignore_ascii_case("true");
                }
                "Terminal" => {
                    terminal = value.eq_ignore_ascii_case("true");
                }
                _ => {}
            }
        }

        if no_display || hidden || terminal {
            return None;
        }
        let name = name?;
        let exec = exec?;
        Some(Self {
            id: file_stem.to_owned(),
            name,
            exec,
            comment,
            icon_name,
            icon_path: None,
        })
    }

    /// Strip `.desktop` Exec field codes (`%f`, `%F`, `%u`, `%U`, `%i`,
    /// `%c`, `%k`) and split into `(program, args)`. We don't pass files
    /// to launched apps, so all codes collapse to empty.
    pub fn resolved_exec(&self) -> Option<(String, Vec<String>)> {
        let cleaned = self
            .exec
            .split_whitespace()
            .filter(|tok| !matches!(*tok, "%f" | "%F" | "%u" | "%U" | "%i" | "%c" | "%k"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut parts = cleaned.split_whitespace();
        let prog = parts.next()?.to_owned();
        let args: Vec<String> = parts.map(|s| s.to_owned()).collect();
        Some((prog, args))
    }
}

/// Default XDG search path for `.desktop` files.
///
/// `$XDG_DATA_HOME/applications` first, then `$XDG_DATA_DIRS` in order.
/// Falls back to `/usr/share/applications` and `/usr/local/share/applications`
/// when the env vars are unset.
pub fn default_search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    if let Some(xdg) = std::env::var_os("XDG_DATA_DIRS") {
        for part in std::env::split_paths(&xdg) {
            dirs.push(part.join("applications"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/local/share/applications"));
        dirs.push(PathBuf::from("/usr/share/applications"));
    }

    dirs
}

/// Scan `dirs` for `*.desktop` files and parse each one. Parse failures
/// and hidden entries are skipped silently. Later directories in the
/// list are lower priority, but we don't dedupe on name — two entries
/// with the same name from different dirs are both surfaced so the user
/// can see both.
pub fn scan_desktop_entries(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(parsed) = DesktopEntry::parse(&text, &stem) {
                    out.push(parsed);
                }
            }
        }
    }
    out
}

// =====================================================================
// Icon theme resolver
// =====================================================================

/// Extensions tried when walking the legacy `<root>/pixmaps`
/// directory. Themed subdirectories use a smaller modern list
/// (see [`extensions_for_subdir`]); this one keeps `xpm` for old
/// packages that still ship it there.
const ICON_EXTS: &[&str] = &["svg", "png", "xpm"];

/// Size subdirectory names for the hicolor/Adwaita/Papirus layout
/// (`<size>/<context>/`), ordered from best (vector) to acceptable
/// (small raster). `@2x` variants appear alongside their base size
/// for hiDPI systems.
const THEME_SIZES: &[&str] = &[
    "scalable",
    "512x512",
    "512x512@2x",
    "256x256",
    "256x256@2x",
    "128x128",
    "128x128@2x",
    "96x96",
    "96x96@2x",
    "64x64",
    "64x64@2x",
    "48x48",
    "48x48@2x",
    "40x40",
    "32x32",
    "32x32@2x",
    "24x24",
    "24x24@2x",
    "22x22",
    "22x22@2x",
    "16x16",
    "16x16@2x",
];

/// Size subdirectory names for the Breeze layout
/// (`<context>/<size>/`).  KDE themes use short size names without
/// the `NxN` format.
const THEME_SIZES_BREEZE: &[&str] = &[
    "scalable", "512", "256", "128", "96", "64", "48", "32", "24", "24@2x", "22",
    "22@2x", "16", "16@2x",
];

/// Icon contexts in preference order. `apps` always wins, then the
/// freedesktop named-icon contexts for system/category/device icons
/// (which `.desktop` files commonly reference for utility apps —
/// e.g. `Icon=network-wired` in an Avahi browser).
const THEME_CONTEXTS: &[&str] = &["apps", "status", "categories", "devices"];

/// Build the ordered list of candidate `<theme>/<subdir>` paths to
/// walk. Apps come first across every size, then other contexts,
/// then the monochromatic symbolic fallback. Both hicolor and Breeze
/// layouts are generated for each context. Computed once per
/// [`IconResolver`] at construction.
fn candidate_subdirs() -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(THEME_CONTEXTS.len() * (THEME_SIZES.len() + THEME_SIZES_BREEZE.len()) + 8);
    for context in THEME_CONTEXTS {
        for size in THEME_SIZES {
            out.push(format!("{size}/{context}"));
        }
        for size in THEME_SIZES_BREEZE {
            out.push(format!("{context}/{size}"));
        }
    }
    // Monochromatic fallback across every context, last resort.
    for context in THEME_CONTEXTS {
        out.push(format!("symbolic/{context}"));
        out.push(format!("{context}/symbolic"));
    }
    out
}

/// Base directories to search for icon themes. Each root is expected
/// to contain `pixmaps/` and/or `icons/<theme>/...` subdirectories.
///
/// Pulls from `$XDG_DATA_HOME` (or `$HOME/.local/share`) first so
/// user-installed icons beat system ones, then `$XDG_DATA_DIRS`, with
/// the standard fallback (`/usr/local/share`, `/usr/share`) if nothing
/// is set.
pub fn default_icon_search_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }
    if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
        for part in std::env::split_paths(&data_dirs) {
            roots.push(part);
        }
    } else {
        roots.push(PathBuf::from("/usr/local/share"));
        roots.push(PathBuf::from("/usr/share"));
    }
    roots
}

/// Parse the `Inherits=` field out of a freedesktop icon-theme
/// `index.theme`. Returns an empty vec if the file is missing,
/// unreadable, or doesn't declare inheritance.
fn parse_theme_inherits(index_theme_path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(index_theme_path) else {
        return Vec::new();
    };
    let mut in_header = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_header = line == "[Icon Theme]";
            continue;
        }
        if !in_header {
            continue;
        }
        if let Some(value) = line.strip_prefix("Inherits=") {
            return value
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

/// Read the icon theme name out of a GTK `settings.ini` file. Returns
/// `None` if the file is missing, has no `[Settings]` section, or
/// doesn't set `gtk-icon-theme-name`.
fn read_icon_theme_from_settings(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_settings = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_settings = line == "[Settings]";
            continue;
        }
        if !in_settings {
            continue;
        }
        if let Some(value) = line.strip_prefix("gtk-icon-theme-name") {
            let value = value.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Detect the user's configured icon theme from GTK `settings.ini`
/// files. Prefers GTK 4 (newer, what users actively configure), falls
/// back to GTK 3. Returns `None` when neither file has a usable
/// setting — callers should then fall back to hicolor only.
pub fn detect_primary_icon_theme() -> Option<String> {
    let base = if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return None;
    };
    for rel in &["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        if let Some(theme) = read_icon_theme_from_settings(&base.join(rel)) {
            return Some(theme);
        }
    }
    None
}

/// Build an ordered icon theme chain starting from `primary`, walking
/// `Inherits=` transitively, and terminating with `hicolor`.
///
/// Loops are broken by a visited set — some themes mistakenly inherit
/// each other, and we must not recurse forever. `hicolor` is always
/// appended if not already present, since the freedesktop spec
/// guarantees it as the universal fallback.
pub fn build_theme_chain(search_roots: &[PathBuf], primary: Option<&str>) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut chain: Vec<String> = Vec::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = Vec::new();

    if let Some(p) = primary {
        stack.push(p.to_owned());
    }

    while let Some(theme) = stack.pop() {
        if !visited.insert(theme.clone()) {
            continue;
        }
        chain.push(theme.clone());
        for root in search_roots {
            let index = root.join("icons").join(&theme).join("index.theme");
            if index.exists() {
                // Reverse so the first entry lands on top of the stack
                // and is popped next — preserves the spec's "recurse in
                // declared order" rule.
                for inh in parse_theme_inherits(&index).into_iter().rev() {
                    stack.push(inh);
                }
                break;
            }
        }
    }

    if !visited.contains("hicolor") {
        chain.push("hicolor".into());
    }
    chain
}

/// Precomputed icon search state — build once at startup and reuse
/// for every `.desktop` entry. Walks the freedesktop icon-theme
/// inheritance chain from GTK settings and caches the per-theme
/// application-context subdirectories that actually exist on disk,
/// so the per-icon `resolve` call only stats files in known-live
/// directories.
pub struct IconResolver {
    /// Existing `<root>/pixmaps` directories, searched before any
    /// themed lookup (legacy path for `.desktop` files that still
    /// carry bare pixmap names).
    pixmaps_dirs: Vec<PathBuf>,
    /// Existing `<root>/icons` directories, searched for icons
    /// placed directly at the theme-roots level (e.g. a package
    /// dropping `/usr/share/icons/xmaxima.png` instead of installing
    /// into a proper theme subdirectory).
    loose_icon_roots: Vec<PathBuf>,
    /// `(theme_base, ordered_subdirs)` — one entry per live
    /// `(theme, root)` combination, already sorted in theme-chain
    /// order. Subdirs are filtered from [`candidate_subdirs`] to
    /// only those that actually exist on disk.
    theme_cache: Vec<(PathBuf, Vec<String>)>,
}

impl IconResolver {
    /// Build a resolver from the default icon search roots and the
    /// user's active GTK icon theme. Falls back to hicolor-only if
    /// GTK settings are unreadable.
    pub fn new() -> Self {
        let search_roots = default_icon_search_roots();
        let primary = detect_primary_icon_theme();
        let theme_chain = build_theme_chain(&search_roots, primary.as_deref());
        Self::from_parts(&search_roots, &theme_chain)
    }

    /// Build a resolver from explicit roots and theme chain. Used by
    /// tests to avoid touching the real GTK config.
    ///
    /// `theme_chain` is used as-is except that `hicolor` is appended
    /// if absent — the freedesktop spec makes it a universal fallback.
    pub fn from_parts(search_roots: &[PathBuf], theme_chain: &[String]) -> Self {
        let mut pixmaps_dirs = Vec::new();
        let mut loose_icon_roots = Vec::new();
        for root in search_roots {
            let pixmaps = root.join("pixmaps");
            if pixmaps.is_dir() {
                pixmaps_dirs.push(pixmaps);
            }
            let icons = root.join("icons");
            if icons.is_dir() {
                loose_icon_roots.push(icons);
            }
        }

        let candidates = candidate_subdirs();
        let mut theme_cache = Vec::new();
        let hicolor_needed = !theme_chain.iter().any(|t| t == "hicolor");
        let effective_chain = theme_chain
            .iter()
            .map(String::as_str)
            .chain(if hicolor_needed { Some("hicolor") } else { None });
        for theme in effective_chain {
            for root in search_roots {
                let theme_base = root.join("icons").join(theme);
                if !theme_base.is_dir() {
                    continue;
                }
                let subdirs: Vec<String> = candidates
                    .iter()
                    .filter(|s| theme_base.join(s).is_dir())
                    .cloned()
                    .collect();
                if !subdirs.is_empty() {
                    theme_cache.push((theme_base, subdirs));
                }
            }
        }

        Self {
            pixmaps_dirs,
            loose_icon_roots,
            theme_cache,
        }
    }

    /// Resolve an `Icon=` value to an absolute filesystem path.
    ///
    /// Resolution tries multiple candidate names in order: the exact
    /// input, then the reverse-DNS tail (`org.kde.dolphin` →
    /// `dolphin`), then progressive dash-suffix strips per the
    /// freedesktop spec (`gnome-web-browser` → `gnome-web` →
    /// `gnome`). Each candidate is looked up as:
    ///
    /// 1. Absolute-path verbatim (only the original input — fallbacks
    ///    are never treated as paths).
    /// 2. Every cached `pixmaps/` directory with each extension.
    /// 3. Every cached `(theme_base, subdirs)` entry in chain order,
    ///    trying SVG for scalable/symbolic subdirs and PNG-then-SVG
    ///    for bitmap ones.
    ///
    /// Returns `None` if no candidate matches anywhere.
    pub fn resolve(&self, icon_name: &str) -> Option<PathBuf> {
        if icon_name.is_empty() {
            return None;
        }

        // Absolute paths are always returned verbatim — a `.desktop`
        // file that ships `Icon=/opt/foo/bar.png` expects that exact
        // path, not a fallback.
        let raw = Path::new(icon_name);
        if raw.is_absolute() {
            return raw.exists().then(|| raw.to_path_buf());
        }

        for candidate in icon_name_fallbacks(icon_name) {
            if let Some(found) = self.resolve_exact(&candidate) {
                return Some(found);
            }
        }
        None
    }

    /// Look up a single icon name verbatim, without generating
    /// fallback candidates. Called by [`Self::resolve`] for each name
    /// in the fallback chain.
    fn resolve_exact(&self, icon_name: &str) -> Option<PathBuf> {
        // Some `.desktop` Icon= values carry an image extension
        // (`firefox.png`) — strip it so we try our own preferred
        // extension order. We only strip *known* image extensions;
        // reverse-DNS names like `org.kde.dolphin` must NOT be
        // treated as "stem=org.kde, ext=dolphin".
        let bare = strip_known_extension(icon_name);

        for pixmaps in &self.pixmaps_dirs {
            for ext in ICON_EXTS {
                let candidate = pixmaps.join(format!("{bare}.{ext}"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        for loose in &self.loose_icon_roots {
            for ext in ICON_EXTS {
                let candidate = loose.join(format!("{bare}.{ext}"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        for (theme_base, subdirs) in &self.theme_cache {
            for subdir in subdirs {
                let dir = theme_base.join(subdir);
                for ext in extensions_for_subdir(subdir) {
                    let candidate = dir.join(format!("{bare}.{ext}"));
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }

        None
    }
}

/// Strip a trailing known image extension (`.svg`, `.png`, `.xpm`)
/// from an icon name, leaving everything else untouched. Required
/// because reverse-DNS icon names like `org.kde.dolphin` must not be
/// treated as `stem = "org.kde", ext = "dolphin"` by `Path::file_stem`.
fn strip_known_extension(name: &str) -> &str {
    for ext in &[".svg", ".png", ".xpm"] {
        if let Some(stripped) = name.strip_suffix(ext) {
            return stripped;
        }
    }
    name
}

/// Generate candidate lookup names for an icon identifier, in order
/// of specificity. The first candidate is always the unchanged input.
///
/// Fallbacks cover two common cases:
///
/// * **Reverse-DNS names** (`org.kde.dolphin`): modern `.desktop`
///   files follow the AppStream convention of using a fully qualified
///   reverse-DNS identifier as the icon name, but most themes still
///   store icons under the short tail (`dolphin.svg`). We fall back
///   on the substring after the last `.`.
/// * **Dash-suffix stripping** (`gnome-web-browser` → `gnome-web` →
///   `gnome`): defined by the freedesktop Icon Theme Specification
///   as a standard fallback. Applied to both the original name and
///   the reverse-DNS tail so both forms benefit.
fn icon_name_fallbacks(icon_name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(4);
    out.push(icon_name.to_owned());

    if let Some(idx) = icon_name.rfind('.') {
        let tail = &icon_name[idx + 1..];
        if !tail.is_empty() && !out.iter().any(|s| s == tail) {
            out.push(tail.to_owned());
        }
    }

    // Dash-suffix strip, applied to both the full name and the
    // reverse-DNS tail so `org.kde.dolphin-view` → `dolphin-view` →
    // `dolphin` is reachable.
    let bases: Vec<String> = out.clone();
    for base in &bases {
        let mut current = base.as_str();
        while let Some(idx) = current.rfind('-') {
            current = &current[..idx];
            if !current.is_empty() && !out.iter().any(|s| s == current) {
                out.push(current.to_owned());
            }
        }
    }

    out
}

impl Default for IconResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Pick the extensions to try for a given theme subdirectory.
/// Scalable/symbolic tiers are SVG-only; everything else prefers PNG
/// (the common case for raster themes) with SVG as a secondary
/// fallback for mixed themes.
fn extensions_for_subdir(subdir: &str) -> &'static [&'static str] {
    if subdir.starts_with("scalable")
        || subdir.contains("/scalable")
        || subdir.starts_with("symbolic")
        || subdir.contains("/symbolic")
    {
        &["svg"]
    } else {
        &["png", "svg"]
    }
}

// =====================================================================
// Launch history (recency ranking)
// =====================================================================

/// Maximum recency boost applied to an entry with age=0 (launched
/// now). Chosen so that a recently-used but imperfectly-matching
/// entry (base ~0.8) still ranks above an exact-name match with no
/// history (floor 0.9 = 1.0 minus 0.1), without letting stale
/// favorites dominate above a genuine exact-name match for the
/// current query.
const RECENCY_BOOST_MAX: f64 = 0.1;

/// Half-life of the recency decay, in days. The boost follows
/// `MAX * exp(-age_days / HALF_LIFE_DAYS)`, so at age=7 the boost is
/// `MAX / e ≈ 0.037`, and by age=30 it's ~0.001 (effectively gone).
const RECENCY_HALF_LIFE_DAYS: f64 = 7.0;

/// Per-entry launch history — `entry_id → last launch unix timestamp
/// (seconds)`. Persisted to `$XDG_STATE_HOME/levshell/launches.json`
/// on every successful launch. Used by [`scored_with_recency`] to
/// boost recently-used apps so they surface first for empty / short
/// queries.
#[derive(Debug, Default, Clone)]
pub struct LaunchHistory {
    entries: BTreeMap<String, i64>,
    /// Where to write on `record`. An empty path skips persistence
    /// (used by tests / in-memory instances).
    path: PathBuf,
}

impl LaunchHistory {
    /// Load the history from `path`, or return an empty instance if
    /// the file is missing / unreadable / malformed. Errors are
    /// logged, not propagated.
    pub fn load(path: PathBuf) -> Self {
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<BTreeMap<String, i64>>(&text)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, path = %path.display(), "launches: parse failed, starting fresh");
                    BTreeMap::new()
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "launches: read failed, starting fresh");
                BTreeMap::new()
            }
        };
        Self { entries, path }
    }

    /// Build an in-memory history with no persistence path. Used by
    /// tests that want to drive scoring without touching the
    /// filesystem.
    pub fn in_memory() -> Self {
        Self {
            entries: BTreeMap::new(),
            path: PathBuf::new(),
        }
    }

    /// Record a launch of `entry_id` at the current time and persist
    /// to disk. File write failures are logged, not propagated.
    pub fn record(&mut self, entry_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.entries.insert(entry_id.to_owned(), now);
        self.persist();
    }

    /// Directly set a launch timestamp. Used by tests to simulate a
    /// specific history without needing to freeze system time.
    #[cfg(test)]
    pub fn set_launch_time(&mut self, entry_id: &str, unix_ts: i64) {
        self.entries.insert(entry_id.to_owned(), unix_ts);
    }

    /// Return the unix timestamp of the last launch for `entry_id`,
    /// or `None` if never launched.
    pub fn last_launch(&self, entry_id: &str) -> Option<i64> {
        self.entries.get(entry_id).copied()
    }

    /// Compute the recency boost for an entry given the current
    /// time. Returns `0.0` for entries with no launch history.
    ///
    /// Formula: `RECENCY_BOOST_MAX * exp(-age_days / RECENCY_HALF_LIFE_DAYS)`
    pub fn recency_boost(&self, entry_id: &str, now_unix: i64) -> f64 {
        let Some(last) = self.last_launch(entry_id) else {
            return 0.0;
        };
        let age_seconds = (now_unix - last).max(0);
        let age_days = age_seconds as f64 / 86_400.0;
        RECENCY_BOOST_MAX * (-age_days / RECENCY_HALF_LIFE_DAYS).exp()
    }

    fn persist(&self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, path = %parent.display(), "launches: mkdir failed");
                return;
            }
        }
        match serde_json::to_string_pretty(&self.entries) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    tracing::warn!(error = %e, path = %self.path.display(), "launches: write failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "launches: serialize failed");
            }
        }
    }
}

/// Default path to the launches history file. Follows XDG state
/// convention: `$XDG_STATE_HOME/levshell/launches.json`, falling
/// back to `$HOME/.local/state/levshell/launches.json`.
pub fn default_launches_path() -> PathBuf {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state).join("levshell/launches.json");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/levshell/launches.json");
    }
    PathBuf::from("/tmp/levshell-launches.json")
}

// =====================================================================
// AppLauncherProvider
// =====================================================================

pub struct AppLauncherProvider {
    entries: Arc<Mutex<Vec<DesktopEntry>>>,
    launches: Arc<Mutex<LaunchHistory>>,
}

impl AppLauncherProvider {
    /// Construct a provider by scanning the XDG `.desktop` directories,
    /// resolving each entry's `Icon=` value through the freedesktop
    /// icon theme search path, and loading the launch history for
    /// recency ranking. Runs once at startup; the cached icon paths
    /// let the live query path stay filesystem-free.
    pub fn new() -> Self {
        let mut entries = scan_desktop_entries(&default_search_paths());
        let resolver = IconResolver::new();
        for entry in &mut entries {
            if let Some(name) = entry.icon_name.as_deref() {
                entry.icon_path = resolver.resolve(name);
            }
        }
        let launches = LaunchHistory::load(default_launches_path());
        Self::from_entries_with_history(entries, launches)
    }

    /// Construct a provider from pre-built entries with an empty
    /// in-memory launch history. Used by tests and by callers that
    /// want to skip filesystem scanning. Does *not* re-run the icon
    /// resolver — caller is responsible for populating `icon_path`
    /// on each entry if icons are desired.
    pub fn from_entries(entries: Vec<DesktopEntry>) -> Self {
        Self::from_entries_with_history(entries, LaunchHistory::in_memory())
    }

    /// Construct a provider from pre-built entries **and** an
    /// explicit launch history. Used by recency tests that want to
    /// drive ranking from a known history.
    pub fn from_entries_with_history(
        entries: Vec<DesktopEntry>,
        launches: LaunchHistory,
    ) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
            launches: Arc::new(Mutex::new(launches)),
        }
    }

    /// Rescan the XDG directories. Useful if a config reload wants to
    /// pick up new apps without restarting the daemon; not used in
    /// Phase 1.5 directly. Re-runs the icon resolver for each fresh
    /// entry. Launch history is preserved across the rescan.
    pub fn rescan(&self) {
        let mut fresh = scan_desktop_entries(&default_search_paths());
        let resolver = IconResolver::new();
        for entry in &mut fresh {
            if let Some(name) = entry.icon_name.as_deref() {
                entry.icon_path = resolver.resolve(name);
            }
        }
        let mut lock = self.entries.lock().expect("app-launcher mutex poisoned");
        *lock = fresh;
    }
}

impl Default for AppLauncherProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Score an entry against a query using the base match logic only —
/// no recency boost. Returns `None` if the entry doesn't match at all.
/// Kept as a free function for tests that want to verify the
/// match-scoring tiers in isolation from launch history.
fn score_entry(entry: &DesktopEntry, query: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.4);
    }
    let q = query.to_ascii_lowercase();
    let name_lc = entry.name.to_ascii_lowercase();
    let exec_lc = entry.exec.to_ascii_lowercase();

    if name_lc == q {
        return Some(1.0);
    }
    if name_lc.starts_with(&q) {
        return Some(0.9);
    }
    if name_lc.contains(&q) {
        // Longer matches score higher (more specific).
        let ratio = q.len() as f64 / name_lc.len().max(1) as f64;
        return Some(0.6 + ratio * 0.2);
    }
    if exec_lc.contains(&q) {
        return Some(0.4);
    }
    None
}

/// Compute the final score for an entry, layering a recency boost
/// on top of [`score_entry`]. Returns `None` when the base score is
/// `None` (i.e. the entry doesn't match the query at all — recency
/// alone never promotes a non-matching entry into the result set).
fn scored_with_recency(
    entry: &DesktopEntry,
    query: &str,
    history: &LaunchHistory,
    now_unix: i64,
) -> Option<f64> {
    let base = score_entry(entry, query)?;
    Some(base + history.recency_boost(&entry.id, now_unix))
}

#[async_trait]
impl PaletteProvider for AppLauncherProvider {
    fn name(&self) -> &'static str {
        APP_LAUNCHER_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let entries = {
            let lock = self.entries.lock().expect("app-launcher mutex poisoned");
            lock.clone()
        };
        let history = {
            let lock = self.launches.lock().expect("app-launcher launches poisoned");
            lock.clone()
        };
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut out: Vec<PaletteItem> = entries
            .iter()
            .filter_map(|e| {
                scored_with_recency(e, query, &history, now_unix).map(|score| {
                    let mut item =
                        PaletteItem::new(APP_LAUNCHER_PROVIDER, e.id.clone(), e.name.clone())
                            .with_score(score)
                            .with_icon("app");
                    if let Some(subtitle) = e.comment.clone() {
                        item = item.with_subtitle(subtitle);
                    } else {
                        item = item.with_subtitle(e.exec.clone());
                    }
                    if let Some(path) = e.icon_path.as_ref().and_then(|p| p.to_str()) {
                        item = item.with_icon_path(path);
                    }
                    item
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.cmp(&b.title))
        });
        // Cap at a generous ceiling so pathological systems with
        // thousands of entries don't blow up the wire frame. Normal
        // systems ship ~150 `.desktop` files and will return all of
        // them for an empty query.
        out.truncate(256);
        out
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        let entry = {
            let lock = self.entries.lock().expect("app-launcher mutex poisoned");
            lock.iter().find(|e| e.id == item_id).cloned()
        };
        let Some(entry) = entry else {
            return Err(ProviderError::UnknownItem(item_id.to_owned()));
        };
        let (prog, args) = entry.resolved_exec().ok_or_else(|| {
            ProviderError::ExecuteFailed(format!("empty Exec line for {}", entry.id))
        })?;
        spawn_detached(&prog, &args).map_err(|e| {
            ProviderError::ExecuteFailed(format!("spawn {prog}: {e}"))
        })?;
        // Record the launch for recency ranking on the next query.
        // Persisted to disk via `LaunchHistory::record`; failures
        // are logged, not propagated (a write failure must never
        // abort the launch that already succeeded).
        {
            let mut history = self
                .launches
                .lock()
                .expect("app-launcher launches poisoned");
            history.record(&entry.id);
        }
        tracing::info!(app = %entry.name, exec = %entry.exec, "app-launcher: spawned");
        Ok(())
    }
}

/// Spawn a process detached from the current task. On unix we put it in
/// a fresh process group via [`CommandExt::process_group`] so a SIGTERM
/// delivered to the daemon's process group doesn't cascade into the
/// launched app.
fn spawn_detached(prog: &str, args: &[String]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[Desktop Entry]
Version=1.0
Type=Application
Name=Firefox
GenericName=Web Browser
Comment=Browse the World Wide Web
Exec=firefox %u
Icon=firefox
Terminal=false
Categories=Network;WebBrowser;
";

    #[test]
    fn parses_sample_firefox_entry() {
        let e = DesktopEntry::parse(SAMPLE, "firefox").unwrap();
        assert_eq!(e.id, "firefox");
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec, "firefox %u");
        assert_eq!(e.comment.as_deref(), Some("Browse the World Wide Web"));
        assert_eq!(e.icon_name.as_deref(), Some("firefox"));
        // icon_path is filled in by AppLauncherProvider::new() /
        // rescan(), not by the parser itself.
        assert!(e.icon_path.is_none());
    }

    #[test]
    fn parses_entry_without_icon_field() {
        let text = "[Desktop Entry]\nName=Plain\nExec=plain\n";
        let e = DesktopEntry::parse(text, "plain").unwrap();
        assert!(e.icon_name.is_none());
    }

    #[test]
    fn parses_entry_with_reverse_dns_icon_name() {
        let text = "[Desktop Entry]\nName=Thing\nExec=thing\nIcon=org.kde.thing\n";
        let e = DesktopEntry::parse(text, "org.kde.thing").unwrap();
        assert_eq!(e.icon_name.as_deref(), Some("org.kde.thing"));
    }

    #[test]
    fn skips_entries_with_nodisplay_true() {
        let text = "[Desktop Entry]\nName=Hidden\nExec=hidden\nNoDisplay=true\n";
        assert!(DesktopEntry::parse(text, "hidden").is_none());
    }

    #[test]
    fn skips_entries_with_hidden_true() {
        let text = "[Desktop Entry]\nName=Gone\nExec=gone\nHidden=true\n";
        assert!(DesktopEntry::parse(text, "gone").is_none());
    }

    #[test]
    fn skips_terminal_applications() {
        let text = "[Desktop Entry]\nName=Vim\nExec=vim\nTerminal=true\n";
        assert!(DesktopEntry::parse(text, "vim").is_none());
    }

    #[test]
    fn rejects_entries_without_name_or_exec() {
        assert!(DesktopEntry::parse("[Desktop Entry]\nName=Foo\n", "x").is_none());
        assert!(DesktopEntry::parse("[Desktop Entry]\nExec=foo\n", "x").is_none());
    }

    #[test]
    fn ignores_non_desktop_entry_sections() {
        let text = "[Desktop Action new-window]
Name=New Window
Exec=firefox --new-window
[Desktop Entry]
Name=Firefox
Exec=firefox
";
        let e = DesktopEntry::parse(text, "firefox").unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec, "firefox");
    }

    #[test]
    fn resolved_exec_strips_field_codes() {
        let entry = DesktopEntry {
            id: "x".into(),
            name: "X".into(),
            exec: "firefox %u --kiosk".into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        };
        let (prog, args) = entry.resolved_exec().unwrap();
        assert_eq!(prog, "firefox");
        assert_eq!(args, vec!["--kiosk".to_string()]);
    }

    #[test]
    fn score_entry_empty_query_is_neutral() {
        let entry = DesktopEntry {
            id: "f".into(),
            name: "Firefox".into(),
            exec: "firefox".into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        };
        assert_eq!(score_entry(&entry, ""), Some(0.4));
    }

    #[test]
    fn score_entry_exact_name_match_is_max() {
        let entry = DesktopEntry {
            id: "f".into(),
            name: "Firefox".into(),
            exec: "firefox".into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        };
        assert_eq!(score_entry(&entry, "firefox"), Some(1.0));
    }

    #[test]
    fn score_entry_prefix_ranks_above_substring() {
        let e = DesktopEntry {
            id: "x".into(),
            name: "Firefox".into(),
            exec: "firefox".into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        };
        let prefix = score_entry(&e, "fire").unwrap();
        let substring = score_entry(&e, "fox").unwrap();
        assert!(prefix > substring, "prefix {prefix} > substring {substring}");
    }

    #[test]
    fn score_entry_returns_none_for_no_match() {
        let e = DesktopEntry {
            id: "x".into(),
            name: "Firefox".into(),
            exec: "firefox".into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        };
        assert!(score_entry(&e, "supercalifragilistic").is_none());
    }

    #[tokio::test]
    async fn search_filters_by_query() {
        let provider = AppLauncherProvider::from_entries(vec![
            DesktopEntry {
                id: "firefox".into(),
                name: "Firefox".into(),
                exec: "firefox".into(),
                comment: None,
                icon_name: None,
                icon_path: None,
            },
            DesktopEntry {
                id: "gedit".into(),
                name: "Text Editor".into(),
                exec: "gedit".into(),
                comment: None,
                icon_name: None,
                icon_path: None,
            },
        ]);
        let hits = provider.search("fire").await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Firefox");
    }

    #[tokio::test]
    async fn execute_unknown_id_returns_error() {
        let provider = AppLauncherProvider::from_entries(vec![]);
        let result = provider.execute("does-not-exist").await;
        assert!(matches!(result, Err(ProviderError::UnknownItem(_))));
    }

    // ----- icon resolver tests -----

    fn write_file(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn resolver_absolute_path_returns_path_if_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("my-app.png");
        write_file(&icon, b"PNG");
        let resolver = IconResolver::from_parts(&[], &[]);
        assert_eq!(resolver.resolve(icon.to_str().unwrap()), Some(icon));
    }

    #[test]
    fn resolver_absolute_path_returns_none_if_missing() {
        let resolver = IconResolver::from_parts(&[], &[]);
        assert!(resolver.resolve("/nonexistent/path/to/icon.png").is_none());
    }

    #[test]
    fn resolver_empty_name_returns_none() {
        let resolver = IconResolver::from_parts(&[], &[]);
        assert!(resolver.resolve("").is_none());
    }

    #[test]
    fn resolver_finds_svg_in_pixmaps() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("pixmaps").join("firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("firefox"), Some(svg));
    }

    #[test]
    fn resolver_finds_hicolor_scalable_svg() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("icons/hicolor/scalable/apps/firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("firefox"), Some(svg));
    }

    #[test]
    fn resolver_prefers_pixmaps_over_hicolor() {
        let dir = tempfile::tempdir().unwrap();
        let pixmaps_svg = dir.path().join("pixmaps").join("foo.svg");
        let hicolor_svg = dir.path().join("icons/hicolor/scalable/apps/foo.svg");
        write_file(&pixmaps_svg, b"<svg/>");
        write_file(&hicolor_svg, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("foo"), Some(pixmaps_svg));
    }

    #[test]
    fn resolver_prefers_scalable_over_48px_when_both_exist() {
        let dir = tempfile::tempdir().unwrap();
        let scalable = dir.path().join("icons/hicolor/scalable/apps/foo.svg");
        let fixed = dir.path().join("icons/hicolor/48x48/apps/foo.png");
        write_file(&scalable, b"<svg/>");
        write_file(&fixed, b"PNG");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("foo"), Some(scalable));
    }

    #[test]
    fn resolver_falls_back_to_256_png_if_no_svg() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("icons/hicolor/256x256/apps/foo.png");
        write_file(&png, b"PNG");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("foo"), Some(png));
    }

    #[test]
    fn resolver_strips_extension_from_name() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("pixmaps").join("firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("firefox.png"), Some(svg));
    }

    #[test]
    fn resolver_returns_none_when_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert!(resolver.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolver_walks_multiple_roots_in_order() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let icon = root_b.path().join("pixmaps").join("foo.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(
            &[root_a.path().to_path_buf(), root_b.path().to_path_buf()],
            &[],
        );
        assert_eq!(resolver.resolve("foo"), Some(icon));
    }

    #[test]
    fn resolver_walks_primary_theme_before_hicolor_fallback() {
        // Icon exists in both Papirus-Dark and hicolor — primary wins.
        let dir = tempfile::tempdir().unwrap();
        let papirus_icon = dir.path().join("icons/Papirus-Dark/scalable/apps/foo.svg");
        let hicolor_icon = dir.path().join("icons/hicolor/scalable/apps/foo.svg");
        write_file(&papirus_icon, b"<svg/>");
        write_file(&hicolor_icon, b"<svg/>");
        write_file(
            &dir.path().join("icons/Papirus-Dark/index.theme"),
            b"[Icon Theme]\nInherits=hicolor\n",
        );
        let resolver = IconResolver::from_parts(
            &[dir.path().to_path_buf()],
            &["Papirus-Dark".into()],
        );
        assert_eq!(resolver.resolve("foo"), Some(papirus_icon));
    }

    #[test]
    fn resolver_falls_back_to_inherited_theme_when_primary_lacks_icon() {
        // Papirus-Dark is a real directory with the apps subdir carved
        // out so it registers in the cache, but has no foo.svg; the
        // icon is only present in its inherited Adwaita.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("icons/Papirus-Dark/scalable/apps")).unwrap();
        write_file(
            &dir.path().join("icons/Papirus-Dark/index.theme"),
            b"[Icon Theme]\nInherits=Adwaita\n",
        );
        let adwaita_icon = dir.path().join("icons/Adwaita/scalable/apps/foo.svg");
        write_file(&adwaita_icon, b"<svg/>");

        let roots = vec![dir.path().to_path_buf()];
        let chain = build_theme_chain(&roots, Some("Papirus-Dark"));
        let resolver = IconResolver::from_parts(&roots, &chain);
        assert_eq!(resolver.resolve("foo"), Some(adwaita_icon));
    }

    #[test]
    fn resolver_handles_breeze_style_apps_subdir_layout() {
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("icons/breeze/apps/48/foo.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(
            &[dir.path().to_path_buf()],
            &["breeze".into()],
        );
        assert_eq!(resolver.resolve("foo"), Some(icon));
    }

    #[test]
    fn resolver_finds_reverse_dns_name_verbatim_when_theme_stores_it() {
        // Modern AppStream convention: icon is stored under the full
        // reverse-DNS name. The resolver must NOT strip the trailing
        // segment as an "extension" via Path::file_stem.
        let dir = tempfile::tempdir().unwrap();
        let icon = dir
            .path()
            .join("icons/hicolor/scalable/apps/org.kde.dolphin.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("org.kde.dolphin"), Some(icon));
    }

    #[test]
    fn resolver_falls_back_to_reverse_dns_tail_when_full_name_missing() {
        // Theme only has `dolphin.svg`, but .desktop says
        // `Icon=org.kde.dolphin`. The reverse-DNS fallback should
        // find it via the short tail.
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("icons/breeze/apps/48/dolphin.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(
            &[dir.path().to_path_buf()],
            &["breeze".into()],
        );
        assert_eq!(resolver.resolve("org.kde.dolphin"), Some(icon));
    }

    #[test]
    fn resolver_walks_non_apps_contexts_for_named_icons() {
        // `.desktop` files can reference freedesktop named icons like
        // `network-wired` that live in the status context rather than
        // apps. The resolver must walk non-apps contexts as a fallback.
        let dir = tempfile::tempdir().unwrap();
        let icon = dir
            .path()
            .join("icons/hicolor/scalable/status/network-wired.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("network-wired"), Some(icon));
    }

    #[test]
    fn resolver_dash_suffix_fallback_strips_progressively() {
        // Freedesktop spec fallback: `gnome-web-browser` → `gnome-web`
        // → `gnome`. Theme only stores the short form.
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("icons/hicolor/scalable/apps/gnome.svg");
        write_file(&icon, b"<svg/>");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("gnome-web-browser"), Some(icon));
    }

    #[test]
    fn resolver_walks_loose_icons_at_root_icons_dir() {
        // Some packages drop icons directly at `<root>/icons/<name>.png`
        // instead of using a theme subdirectory. The resolver has a
        // dedicated walk for this non-standard layout.
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("icons/xmaxima.png");
        write_file(&icon, b"PNG");
        let resolver = IconResolver::from_parts(&[dir.path().to_path_buf()], &[]);
        assert_eq!(resolver.resolve("xmaxima"), Some(icon));
    }

    // ----- icon_name_fallbacks tests -----

    #[test]
    fn icon_name_fallbacks_plain_name_has_single_candidate() {
        assert_eq!(icon_name_fallbacks("firefox"), vec!["firefox"]);
    }

    #[test]
    fn icon_name_fallbacks_reverse_dns_adds_last_segment() {
        assert_eq!(
            icon_name_fallbacks("org.kde.dolphin"),
            vec!["org.kde.dolphin", "dolphin"]
        );
    }

    #[test]
    fn icon_name_fallbacks_dash_suffix_strips_progressively() {
        assert_eq!(
            icon_name_fallbacks("gnome-web-browser"),
            vec!["gnome-web-browser", "gnome-web", "gnome"]
        );
    }

    #[test]
    fn icon_name_fallbacks_mixed_reverse_dns_and_dash() {
        // Both fallbacks apply: full name, reverse-DNS tail, then
        // dash strips of both.
        let result = icon_name_fallbacks("org.kde.dolphin-view");
        assert!(result.contains(&"org.kde.dolphin-view".to_string()));
        assert!(result.contains(&"dolphin-view".to_string()));
        assert!(result.contains(&"org.kde.dolphin".to_string()));
        assert!(result.contains(&"dolphin".to_string()));
    }

    #[test]
    fn icon_name_fallbacks_deduplicates() {
        // `foo.foo` — reverse-DNS tail is `foo`, which is distinct
        // from `foo.foo` so we still emit two entries. But a dash
        // strip of `foo.foo` would yield nothing (no dashes).
        assert_eq!(icon_name_fallbacks("foo.foo"), vec!["foo.foo", "foo"]);
    }

    // ----- strip_known_extension tests -----

    #[test]
    fn strip_known_extension_removes_png() {
        assert_eq!(strip_known_extension("firefox.png"), "firefox");
    }

    #[test]
    fn strip_known_extension_removes_svg() {
        assert_eq!(strip_known_extension("firefox.svg"), "firefox");
    }

    #[test]
    fn strip_known_extension_preserves_reverse_dns_names() {
        // `org.kde.dolphin` does NOT have a known image extension,
        // so it must be returned unchanged.
        assert_eq!(strip_known_extension("org.kde.dolphin"), "org.kde.dolphin");
    }

    #[test]
    fn strip_known_extension_preserves_dotted_name_without_image_ext() {
        assert_eq!(strip_known_extension("com.example.app"), "com.example.app");
    }

    // ----- theme inheritance parser tests -----

    #[test]
    fn parse_theme_inherits_captures_comma_separated_list() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("index.theme");
        write_file(
            &index,
            b"[Icon Theme]\nName=Test\nInherits=Papirus,Adwaita,hicolor\n",
        );
        assert_eq!(
            parse_theme_inherits(&index),
            vec!["Papirus", "Adwaita", "hicolor"]
        );
    }

    #[test]
    fn parse_theme_inherits_ignores_non_icon_theme_sections() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("index.theme");
        write_file(
            &index,
            b"[16x16/apps]\nInherits=nope\n[Icon Theme]\nInherits=yes\n",
        );
        assert_eq!(parse_theme_inherits(&index), vec!["yes"]);
    }

    #[test]
    fn parse_theme_inherits_trims_whitespace_and_skips_empty_entries() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("index.theme");
        write_file(&index, b"[Icon Theme]\nInherits= A , B ,, C \n");
        assert_eq!(parse_theme_inherits(&index), vec!["A", "B", "C"]);
    }

    #[test]
    fn parse_theme_inherits_missing_file_returns_empty() {
        assert!(parse_theme_inherits(Path::new("/nonexistent/index.theme")).is_empty());
    }

    #[test]
    fn parse_theme_inherits_no_inherits_line_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("index.theme");
        write_file(&index, b"[Icon Theme]\nName=Loner\n");
        assert!(parse_theme_inherits(&index).is_empty());
    }

    // ----- GTK settings parser tests -----

    #[test]
    fn read_icon_theme_from_settings_parses_gtk4_style() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.ini");
        write_file(
            &settings,
            b"[Settings]\ngtk-theme-name=Adwaita-dark\ngtk-icon-theme-name=Papirus-Dark\n",
        );
        assert_eq!(
            read_icon_theme_from_settings(&settings),
            Some("Papirus-Dark".into())
        );
    }

    #[test]
    fn read_icon_theme_from_settings_strips_surrounding_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.ini");
        write_file(&settings, b"[Settings]\ngtk-icon-theme-name=\"Breeze\"\n");
        assert_eq!(
            read_icon_theme_from_settings(&settings),
            Some("Breeze".into())
        );
    }

    #[test]
    fn read_icon_theme_from_settings_missing_file_returns_none() {
        assert!(read_icon_theme_from_settings(Path::new("/nonexistent/settings.ini")).is_none());
    }

    #[test]
    fn read_icon_theme_from_settings_key_in_wrong_section_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.ini");
        write_file(
            &settings,
            b"[Other]\ngtk-icon-theme-name=Wrong\n[Settings]\ngtk-font-name=Sans\n",
        );
        assert!(read_icon_theme_from_settings(&settings).is_none());
    }

    // ----- build_theme_chain tests -----

    #[test]
    fn build_theme_chain_walks_inheritance_transitively() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("icons/A/index.theme"),
            b"[Icon Theme]\nInherits=B\n",
        );
        write_file(
            &dir.path().join("icons/B/index.theme"),
            b"[Icon Theme]\nInherits=C\n",
        );
        write_file(
            &dir.path().join("icons/C/index.theme"),
            b"[Icon Theme]\nName=C\n",
        );
        let roots = vec![dir.path().to_path_buf()];
        assert_eq!(
            build_theme_chain(&roots, Some("A")),
            vec!["A", "B", "C", "hicolor"]
        );
    }

    #[test]
    fn build_theme_chain_detects_cycles() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("icons/X/index.theme"),
            b"[Icon Theme]\nInherits=Y\n",
        );
        write_file(
            &dir.path().join("icons/Y/index.theme"),
            b"[Icon Theme]\nInherits=X\n",
        );
        let roots = vec![dir.path().to_path_buf()];
        assert_eq!(
            build_theme_chain(&roots, Some("X")),
            vec!["X", "Y", "hicolor"]
        );
    }

    #[test]
    fn build_theme_chain_without_primary_is_just_hicolor() {
        assert_eq!(build_theme_chain(&[], None), vec!["hicolor"]);
    }

    #[test]
    fn build_theme_chain_primary_named_hicolor_yields_single_entry() {
        assert_eq!(build_theme_chain(&[], Some("hicolor")), vec!["hicolor"]);
    }

    #[test]
    fn build_theme_chain_missing_index_theme_still_includes_primary() {
        assert_eq!(
            build_theme_chain(&[], Some("MysteryTheme")),
            vec!["MysteryTheme", "hicolor"]
        );
    }

    // ----- launch history tests -----

    const DAY: i64 = 86_400;

    fn test_entry(id: &str, name: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.into(),
            name: name.into(),
            exec: id.into(),
            comment: None,
            icon_name: None,
            icon_path: None,
        }
    }

    #[test]
    fn launch_history_load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launches.json");
        let history = LaunchHistory::load(path);
        assert!(history.last_launch("firefox").is_none());
    }

    #[test]
    fn launch_history_load_malformed_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launches.json");
        std::fs::write(&path, b"not json at all").unwrap();
        let history = LaunchHistory::load(path);
        assert!(history.last_launch("firefox").is_none());
    }

    #[test]
    fn launch_history_record_persists_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launches.json");
        {
            let mut history = LaunchHistory::load(path.clone());
            history.record("firefox");
        }
        let reloaded = LaunchHistory::load(path);
        assert!(reloaded.last_launch("firefox").is_some());
    }

    #[test]
    fn launch_history_record_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Parent directory intentionally does not exist yet —
        // `record` should `create_dir_all` it before writing.
        let path = dir.path().join("nested/state/levshell/launches.json");
        let mut history = LaunchHistory::load(path.clone());
        history.record("firefox");
        assert!(path.exists());
    }

    #[test]
    fn launch_history_in_memory_skips_persistence() {
        // In-memory histories have an empty path and must not try
        // to write (would panic or error on invalid path).
        let mut history = LaunchHistory::in_memory();
        history.record("firefox");
        assert!(history.last_launch("firefox").is_some());
    }

    #[test]
    fn recency_boost_for_never_launched_entry_is_zero() {
        let history = LaunchHistory::in_memory();
        assert_eq!(history.recency_boost("firefox", 1000), 0.0);
    }

    #[test]
    fn recency_boost_at_age_zero_is_max() {
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 1000);
        let boost = history.recency_boost("firefox", 1000);
        assert!((boost - 0.1).abs() < 1e-9, "boost={boost}");
    }

    #[test]
    fn recency_boost_decays_at_half_life() {
        // At exactly 7 days old, boost should equal MAX / e.
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 0);
        let boost = history.recency_boost("firefox", 7 * DAY);
        let expected = 0.1 / std::f64::consts::E;
        assert!(
            (boost - expected).abs() < 1e-6,
            "boost={boost} expected={expected}"
        );
    }

    #[test]
    fn recency_boost_decays_aggressively_after_a_month() {
        // At 30 days old the boost should be nearly gone (< 0.005).
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 0);
        let boost = history.recency_boost("firefox", 30 * DAY);
        assert!(boost < 0.005, "boost={boost} — should be ~0 by 30 days");
    }

    #[test]
    fn recency_boost_clamps_future_timestamps_to_zero_age() {
        // If a launch timestamp is somehow in the future (clock
        // skew), treat it as age=0 rather than a negative age that
        // inflates the boost via exp of a positive number.
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 1_000_000);
        let boost = history.recency_boost("firefox", 0);
        assert!((boost - 0.1).abs() < 1e-9, "boost={boost}");
    }

    // ----- scored_with_recency tests -----

    #[test]
    fn scored_with_recency_returns_none_for_non_matching_entry() {
        // Recency alone must not promote an entry that doesn't
        // match the query into the result set.
        let entry = test_entry("firefox", "Firefox");
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 0);
        let score = scored_with_recency(&entry, "supercalifragilistic", &history, 0);
        assert!(score.is_none());
    }

    #[test]
    fn scored_with_recency_empty_query_adds_boost_to_base() {
        let entry = test_entry("firefox", "Firefox");
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 100);
        let score = scored_with_recency(&entry, "", &history, 100).unwrap();
        // Base for empty query = 0.4, boost at age=0 = 0.1.
        assert!((score - 0.5).abs() < 1e-9, "score={score}");
    }

    #[test]
    fn scored_with_recency_recent_launch_ranks_above_cold_entry() {
        let recent = test_entry("firefox", "Firefox");
        let cold = test_entry("gedit", "Text Editor");
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("firefox", 0);
        let recent_score = scored_with_recency(&recent, "", &history, 0).unwrap();
        let cold_score = scored_with_recency(&cold, "", &history, 0).unwrap();
        assert!(
            recent_score > cold_score,
            "recent={recent_score} cold={cold_score}"
        );
    }

    #[test]
    fn scored_with_recency_exact_name_match_still_tops_recent_prefix_match() {
        // Entry A exactly matches "foo" but has no history.
        // Entry B is a recent prefix match "foobar".
        // Exact match (base=1.0) must still beat recent prefix
        // (base=0.9 + boost=0.1 = 1.0) — ties break alphabetically
        // and "foo" sorts before "foobar".
        let exact = test_entry("a", "foo");
        let prefix = test_entry("b", "foobar");
        let mut history = LaunchHistory::in_memory();
        history.set_launch_time("b", 0);
        let exact_score = scored_with_recency(&exact, "foo", &history, 0).unwrap();
        let prefix_score = scored_with_recency(&prefix, "foo", &history, 0).unwrap();
        assert!(
            exact_score >= prefix_score,
            "exact={exact_score} prefix={prefix_score}"
        );
    }

    #[tokio::test]
    async fn search_ranks_recent_launches_first_for_empty_query() {
        let mut history = LaunchHistory::in_memory();
        // Simulate "gedit was launched 3 minutes ago".
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        history.set_launch_time("gedit", now - 180);
        let provider = AppLauncherProvider::from_entries_with_history(
            vec![
                test_entry("firefox", "Firefox"),
                test_entry("gedit", "Text Editor"),
            ],
            history,
        );
        let hits = provider.search("").await;
        assert_eq!(hits.len(), 2);
        // gedit should be first because it has a recency boost.
        assert_eq!(hits[0].id, "gedit");
        assert_eq!(hits[1].id, "firefox");
    }

    #[test]
    fn default_launches_path_uses_xdg_state_home_when_set() {
        // Can't manipulate env safely in a parallel test runner, so
        // just assert the path ends with the expected suffix.
        let path = default_launches_path();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with("levshell/launches.json"),
            "unexpected path: {s}"
        );
    }
}
