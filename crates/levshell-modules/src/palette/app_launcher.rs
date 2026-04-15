//! App launcher provider.
//!
//! Scans well-known XDG application directories for `.desktop` files,
//! parses a minimal subset of the Desktop Entry Specification, and matches
//! user queries against `Name` and `Exec`. Selected items are spawned as
//! detached child processes.
//!
//! Phase 1.5 intentionally ignores:
//! * `Categories`, `Keywords`, `GenericName`
//! * `Icon` (we emit a static glyph instead)
//! * Localized names (`Name[en_US]=…`)
//! * Terminal applications (`Terminal=true`)
//! * Actions (`Actions=…`)
//!
//! The parser is ~40 lines and correct for the 95% of entries that just
//! set `[Desktop Entry]`, `Name`, `Exec`, and optionally
//! `Comment`/`NoDisplay`/`Hidden`. Anything exotic falls through to
//! "treat as regular app".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
                "Name" => {
                    if name.is_none() {
                        name = Some(value.to_owned());
                    }
                }
                "Exec" => {
                    if exec.is_none() {
                        exec = Some(value.to_owned());
                    }
                }
                "Comment" => {
                    if comment.is_none() {
                        comment = Some(value.to_owned());
                    }
                }
                "Icon" => {
                    if icon_name.is_none() && !value.is_empty() {
                        icon_name = Some(value.to_owned());
                    }
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

/// Hicolor icon-theme size subdirectories we search, in descending
/// preference order. Bigger sources downscale to 24px better than
/// blurrier smaller ones.
const HICOLOR_SIZES: &[&str] = &[
    "scalable", // vector SVG, always first
    "512x512",
    "256x256",
    "128x128",
    "64x64",
    "48x48",
    "32x32",
];

/// Icon file extensions we try, in order. SVG first so vector icons
/// beat rasterized siblings.
const ICON_EXTS: &[&str] = &["svg", "png", "xpm"];

/// Base directories to search for icon themes. Each root is expected
/// to contain `pixmaps/` and/or `icons/hicolor/...` subdirectories.
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

/// Resolve an `Icon=` value to an absolute filesystem path, walking
/// the freedesktop icon theme search path. Phase 1.6 implements a
/// minimal resolver that checks only the **hicolor** theme (every
/// theme's ultimate fallback per spec) plus the legacy `/usr/share/pixmaps`
/// directory. Per-theme lookups (Papirus, Adwaita, Breeze, …) are a
/// future extension.
///
/// Resolution order:
///
///   1. If `icon_name` is absolute and the file exists, return it as-is.
///   2. For each `search_root` (in the order from `default_icon_search_roots`):
///      a. `<root>/pixmaps/<name>.{svg,png,xpm}`
///      b. `<root>/icons/hicolor/scalable/apps/<name>.svg`
///      c. `<root>/icons/hicolor/{512,256,128,64,48,32}x.../apps/<name>.png`
///   3. Return `None` if nothing matches.
///
/// This is called **once per entry at scan time** ([`AppLauncherProvider::new`])
/// and the resolved paths are cached on the `DesktopEntry`, so the live
/// query path never touches the filesystem.
pub fn resolve_icon(icon_name: &str, search_roots: &[PathBuf]) -> Option<PathBuf> {
    if icon_name.is_empty() {
        return None;
    }

    // Case 1: absolute path.
    let raw = Path::new(icon_name);
    if raw.is_absolute() {
        return raw.exists().then(|| raw.to_path_buf());
    }

    // Case 2: theme / pixmaps lookup. If the caller passed a name with
    // an extension already (`firefox.png`), strip it so we try our
    // own extension order on the bare name.
    let bare = raw
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(icon_name);

    for root in search_roots {
        // 2a: legacy pixmaps directory.
        let pixmaps = root.join("pixmaps");
        for ext in ICON_EXTS {
            let candidate = pixmaps.join(format!("{bare}.{ext}"));
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // 2b + 2c: hicolor theme, scalable first, then PNG size ladder.
        let hicolor = root.join("icons").join("hicolor");
        for size in HICOLOR_SIZES {
            // Pick the extension appropriate for this size tier: SVG
            // for scalable, PNG for bitmap sizes.
            let exts: &[&str] = if *size == "scalable" { &["svg"] } else { &["png"] };
            for ext in exts {
                let candidate = hicolor.join(size).join("apps").join(format!("{bare}.{ext}"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

pub struct AppLauncherProvider {
    entries: Arc<Mutex<Vec<DesktopEntry>>>,
}

impl AppLauncherProvider {
    /// Construct a provider by scanning the XDG `.desktop` directories
    /// **and** resolving each entry's `Icon=` value through the
    /// freedesktop icon theme search path. Runs once at startup; the
    /// cached icon paths let the live query path stay filesystem-free.
    pub fn new() -> Self {
        let mut entries = scan_desktop_entries(&default_search_paths());
        let icon_roots = default_icon_search_roots();
        for entry in &mut entries {
            if let Some(name) = entry.icon_name.as_deref() {
                entry.icon_path = resolve_icon(name, &icon_roots);
            }
        }
        Self::from_entries(entries)
    }

    /// Construct a provider from pre-built entries. Used by tests and
    /// by callers that want to skip filesystem scanning. Does *not*
    /// re-run the icon resolver — caller is responsible for populating
    /// `icon_path` on each entry if icons are desired.
    pub fn from_entries(entries: Vec<DesktopEntry>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
        }
    }

    /// Rescan the XDG directories. Useful if a config reload wants to
    /// pick up new apps without restarting the daemon; not used in
    /// Phase 1.5 directly. Re-runs the icon resolver for each fresh
    /// entry.
    pub fn rescan(&self) {
        let mut fresh = scan_desktop_entries(&default_search_paths());
        let icon_roots = default_icon_search_roots();
        for entry in &mut fresh {
            if let Some(name) = entry.icon_name.as_deref() {
                entry.icon_path = resolve_icon(name, &icon_roots);
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

/// Score an entry against a query. Returns `None` if the entry doesn't
/// match at all.
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
        let mut out: Vec<PaletteItem> = entries
            .iter()
            .filter_map(|e| {
                score_entry(e, query).map(|score| {
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

    // ----- resolve_icon tests -----

    fn write_file(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn resolve_icon_absolute_path_returns_path_if_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let icon = dir.path().join("my-app.png");
        write_file(&icon, b"PNG");
        let resolved = resolve_icon(icon.to_str().unwrap(), &[]);
        assert_eq!(resolved, Some(icon));
    }

    #[test]
    fn resolve_icon_absolute_path_returns_none_if_missing() {
        let resolved = resolve_icon("/nonexistent/path/to/icon.png", &[]);
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_icon_empty_name_returns_none() {
        let resolved = resolve_icon("", &[]);
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_icon_finds_svg_in_pixmaps() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("pixmaps").join("firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolved = resolve_icon("firefox", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(svg));
    }

    #[test]
    fn resolve_icon_finds_hicolor_scalable_svg() {
        let dir = tempfile::tempdir().unwrap();
        let svg = dir
            .path()
            .join("icons/hicolor/scalable/apps/firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolved = resolve_icon("firefox", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(svg));
    }

    #[test]
    fn resolve_icon_prefers_pixmaps_over_hicolor() {
        // pixmaps is checked before hicolor in the search order.
        let dir = tempfile::tempdir().unwrap();
        let pixmaps_svg = dir.path().join("pixmaps").join("foo.svg");
        let hicolor_svg = dir
            .path()
            .join("icons/hicolor/scalable/apps/foo.svg");
        write_file(&pixmaps_svg, b"<svg/>");
        write_file(&hicolor_svg, b"<svg/>");
        let resolved = resolve_icon("foo", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(pixmaps_svg));
    }

    #[test]
    fn resolve_icon_prefers_scalable_over_48px_when_both_exist() {
        let dir = tempfile::tempdir().unwrap();
        let scalable = dir
            .path()
            .join("icons/hicolor/scalable/apps/foo.svg");
        let fixed = dir.path().join("icons/hicolor/48x48/apps/foo.png");
        write_file(&scalable, b"<svg/>");
        write_file(&fixed, b"PNG");
        let resolved = resolve_icon("foo", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(scalable));
    }

    #[test]
    fn resolve_icon_falls_back_to_256_png_if_no_svg() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("icons/hicolor/256x256/apps/foo.png");
        write_file(&png, b"PNG");
        let resolved = resolve_icon("foo", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(png));
    }

    #[test]
    fn resolve_icon_strips_extension_from_name() {
        // Passing "firefox.png" should still resolve to the bare
        // "firefox" lookup (some Icon= fields carry extensions).
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("pixmaps").join("firefox.svg");
        write_file(&svg, b"<svg/>");
        let resolved = resolve_icon("firefox.png", &[dir.path().to_path_buf()]);
        assert_eq!(resolved, Some(svg));
    }

    #[test]
    fn resolve_icon_returns_none_when_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_icon("nonexistent", &[dir.path().to_path_buf()]);
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_icon_walks_multiple_roots_in_order() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        // Put the icon in root_b only; root_a has nothing.
        let icon = root_b.path().join("pixmaps").join("foo.svg");
        write_file(&icon, b"<svg/>");
        let resolved = resolve_icon(
            "foo",
            &[root_a.path().to_path_buf(), root_b.path().to_path_buf()],
        );
        assert_eq!(resolved, Some(icon));
    }
}
