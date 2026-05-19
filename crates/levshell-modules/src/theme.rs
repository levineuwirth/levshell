//! Theme service (spec design doc §11).
//!
//! Loads a theme TOML from `~/.config/levshell/themes/<name>.toml`,
//! publishes a full [`DaemonMessage::Theme`] payload over IPC so the
//! shell's Theme.qml can apply overrides, and fires
//! [`Event::ThemeActivated`] on the bus for any module that wants
//! to react to theme switches (future: Sway-border propagator, GTK
//! sync).
//!
//! **Not a `Module`.** It's a shared service held behind an
//! `Arc<ThemeService>` on the daemon's `SharedState`, same pattern
//! as `ProjectRegistry`. Per-ctl-request it loads + publishes
//! synchronously; no tick, no per-tick state.
//!
//! The shell's per-connection [`WidgetPublisher`] is set via
//! [`ThemeService::attach_publisher`] after the shell connects.
//! Before that the service still handles `query` / `list` cleanly;
//! `set` / `toggle_mode` are buffered — the theme applies to
//! `self.active`, and the publisher push is skipped with a warn.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use levshell_config::{
    list_themes, load_theme, BarTokens, ColorTokens, HealthTokens, ThemeFile,
    TypographyTokens,
};
use levshell_core::{Event, EventBus};
use levshell_ipc::{
    DaemonMessage, PresentationMode, ThemeBar, ThemeColors, ThemeHealth, ThemePayload,
    ThemeSnapshot, ThemeTypography, WidgetPublisher,
};

pub const MODULE_NAME: &str = "theme";

/// Theme activated at daemon start unless a later
/// `levshell-ctl theme set` overrides. Users who want a different
/// default add `exec_always levshell-ctl theme set <name>` to their
/// Sway config.
pub const DEFAULT_THEME_NAME: &str = "warm-dark";

#[derive(Debug)]
struct Inner {
    active: Option<ThemeFile>,
    /// Stem the active theme was loaded by (the `<name>.toml` id, not
    /// the display `meta.name`). Pair fields reference stems, so
    /// `toggle_mode` needs this to detect a self-pair.
    active_name: Option<String>,
    publisher: Option<WidgetPublisher>,
    /// Presentation mode (spec §2.18) — muted non-critical surfaces.
    presentation: bool,
}

/// Shared service. Cheap to clone via `Arc`.
pub struct ThemeService {
    inner: Mutex<Inner>,
    themes_dir: Option<PathBuf>,
    bus: EventBus,
    /// When true, `activate` also pushes the palette to Sway
    /// (`client.focused` colours) and the GTK/Qt portal
    /// (`color-scheme`). Off in unit tests so they don't spawn
    /// `swaymsg`/`gsettings`.
    propagate: bool,
}

impl ThemeService {
    pub fn new(themes_dir: Option<PathBuf>, bus: EventBus) -> Self {
        Self {
            inner: Mutex::new(Inner {
                active: None,
                active_name: None,
                publisher: None,
                presentation: false,
            }),
            themes_dir,
            bus,
            propagate: true,
        }
    }

    /// Disable Sway/GTK propagation (tests). Production keeps the
    /// default `true`.
    pub fn with_propagation(mut self, on: bool) -> Self {
        self.propagate = on;
        self
    }

    /// Bind the shell's per-connection [`WidgetPublisher`]. Called
    /// by the daemon after handshake completes. Immediately pushes
    /// the currently-active theme (if any) to catch late-joining
    /// shells up with the daemon's state.
    pub fn attach_publisher(&self, publisher: WidgetPublisher) {
        let payload = {
            let mut guard = self.inner.lock().expect("theme service lock poisoned");
            guard.publisher = Some(publisher.clone());
            guard.active.as_ref().map(file_to_payload)
        };
        if let Some(payload) = payload {
            if let Err(e) = publisher.try_send(DaemonMessage::Theme(Box::new(payload))) {
                tracing::warn!(
                    error = %e,
                    "theme: failed to push active theme on shell connect"
                );
            }
        }
    }

    /// Currently-active snapshot, or `None` when no theme has been
    /// successfully loaded since daemon start.
    pub fn snapshot(&self) -> Option<ThemeSnapshot> {
        let guard = self.inner.lock().expect("theme service lock poisoned");
        guard.active.as_ref().map(theme_snapshot)
    }

    /// Enumerate available theme names from the configured dir.
    pub fn list(&self) -> Vec<String> {
        self.themes_dir
            .as_deref()
            .map(list_themes)
            .unwrap_or_default()
    }

    /// Activate `name`. Loads the TOML, publishes a full
    /// [`DaemonMessage::Theme`] to the shell (if attached), and
    /// emits [`Event::ThemeActivated`] on the bus. Returns the new
    /// snapshot on success.
    pub fn activate(&self, name: &str) -> Result<ThemeSnapshot, String> {
        let Some(dir) = self.themes_dir.as_deref() else {
            return Err("no themes directory configured".into());
        };
        let theme = load_theme(dir, name).map_err(|e| e.to_string())?;
        let payload = file_to_payload(&theme);
        let snapshot = theme_snapshot(&theme);

        // Take a clone of the publisher under lock, then drop the
        // lock before sending so try_send can't deadlock if the
        // publisher callback ever tries to re-enter (it doesn't
        // today, but defensive).
        let publisher = {
            let mut guard = self.inner.lock().expect("theme service lock poisoned");
            guard.active = Some(theme);
            guard.active_name = Some(name.to_owned());
            guard.publisher.clone()
        };
        if let Some(p) = publisher {
            if let Err(e) = p.try_send(DaemonMessage::Theme(Box::new(payload))) {
                tracing::warn!(error = %e, "theme: failed to publish ThemePayload");
            }
        } else {
            tracing::debug!(
                theme = %snapshot.name,
                "theme: activated without a shell connection; payload buffered"
            );
        }

        self.bus.publish(Event::ThemeActivated {
            name: snapshot.name.clone(),
            variant: snapshot.variant.clone(),
        });

        // Propagate to the compositor and the GTK/Qt portal so the
        // whole desktop tracks the bar (spec §2.18). Fail-soft: a
        // missing `swaymsg`/`gsettings` is logged, never fatal — the
        // bar itself is already themed via the IPC payload above.
        if self.propagate {
            let guard = self.inner.lock().expect("theme service lock poisoned");
            if let Some(theme) = guard.active.as_ref() {
                propagate_to_desktop(theme);
            }
        }

        tracing::info!(
            theme = %snapshot.name,
            variant = %snapshot.variant,
            "theme: activated"
        );
        Ok(snapshot)
    }

    /// Switch to the current theme's paired variant. Returns an
    /// error when there's no active theme or the current one
    /// doesn't declare a `light_pair` / `dark_pair`.
    pub fn toggle_mode(&self) -> Result<ThemeSnapshot, String> {
        let (pair_name, current) = {
            let guard = self.inner.lock().expect("theme service lock poisoned");
            let Some(active) = guard.active.as_ref() else {
                return Err("no active theme to toggle from".into());
            };
            let pair = match active.meta.variant.as_str() {
                "dark" => active.meta.light_pair.clone(),
                "light" => active.meta.dark_pair.clone(),
                other => {
                    return Err(format!("active theme has invalid variant {other:?}"));
                }
            };
            (pair, guard.active_name.clone())
        };
        let Some(pair) = pair_name else {
            return Err(
                "active theme does not declare a light_pair / dark_pair to toggle to".into(),
            );
        };
        // A theme naming itself as its own pair would just re-activate
        // the same theme (no variant change). Reject it loudly rather
        // than silently no-op.
        if current.as_deref() == Some(pair.as_str()) {
            return Err(format!(
                "active theme's pair points at itself ({pair:?}) — not a light/dark toggle"
            ));
        }
        self.activate(&pair)
    }

    /// Toggle / set presentation mode (spec §2.18). `arg` is `"on"`,
    /// `"off"`, or `"toggle"` / `None` (flip). Pushes
    /// [`DaemonMessage::PresentationMode`] to the shell so it hides the
    /// nudge toast + overlays, and fires
    /// [`Event::PresentationModeChanged`] so the notifications module
    /// drops non-critical desktop notifications. Returns the new state.
    pub fn set_presentation(&self, arg: Option<&str>) -> bool {
        let (on, publisher) = {
            let mut guard = self.inner.lock().expect("theme service lock poisoned");
            let on = match arg {
                Some("on") => true,
                Some("off") => false,
                _ => !guard.presentation, // "toggle" / None / unknown
            };
            guard.presentation = on;
            (on, guard.publisher.clone())
        };
        if let Some(p) = publisher {
            if let Err(e) =
                p.try_send(DaemonMessage::PresentationMode(PresentationMode { on }))
            {
                tracing::warn!(error = %e, "theme: failed to push PresentationMode");
            }
        }
        self.bus.publish(Event::PresentationModeChanged { on });
        tracing::info!(on, "theme: presentation mode");
        on
    }

    /// Current presentation-mode state.
    pub fn presentation(&self) -> bool {
        self.inner
            .lock()
            .expect("theme service lock poisoned")
            .presentation
    }

    /// Activate [`DEFAULT_THEME_NAME`] if it parses, otherwise log
    /// and leave `active` as `None`. Called from daemon boot before
    /// any shell connects; a later `ThemeService::attach_publisher`
    /// pushes whatever landed here to the shell.
    pub fn load_default(&self) {
        if self.themes_dir.is_none() {
            tracing::debug!(
                "theme: no themes directory; shell will use built-in Theme.qml defaults"
            );
            return;
        }
        if let Err(e) = self.activate(DEFAULT_THEME_NAME) {
            tracing::warn!(
                theme = DEFAULT_THEME_NAME,
                error = %e,
                "theme: couldn't activate default on boot; shell will use built-in Theme.qml defaults"
            );
        }
    }

    /// Re-activate the currently-active theme from disk — the
    /// hot-reload watcher calls this when a theme TOML changes so an
    /// edit to the active theme re-pushes its payload live. No-op
    /// (`Ok`) when nothing is active yet.
    pub fn reload_active(&self) -> Result<(), String> {
        let name = {
            let guard = self.inner.lock().expect("theme service lock poisoned");
            guard.active_name.clone()
        };
        match name {
            Some(n) => self.activate(&n).map(|_| ()),
            None => Ok(()),
        }
    }
}

fn theme_snapshot(theme: &ThemeFile) -> ThemeSnapshot {
    ThemeSnapshot {
        name: theme.meta.name.clone(),
        variant: theme.meta.variant.clone(),
        light_pair: theme.meta.light_pair.clone(),
        dark_pair: theme.meta.dark_pair.clone(),
    }
}

/// Translate a parsed [`ThemeFile`] into the wire-shape
/// [`ThemePayload`]. 1:1 pass-through — overrides stay `Option<T>`.
pub(crate) fn file_to_payload(theme: &ThemeFile) -> ThemePayload {
    ThemePayload {
        name: theme.meta.name.clone(),
        variant: theme.meta.variant.clone(),
        light_pair: theme.meta.light_pair.clone(),
        dark_pair: theme.meta.dark_pair.clone(),
        colors: colors_to_wire(&theme.colors),
        health: theme.health.as_ref().map(health_to_wire).unwrap_or_default(),
        bar: theme.bar.as_ref().map(bar_to_wire).unwrap_or_default(),
        typography: theme
            .typography
            .as_ref()
            .map(typography_to_wire)
            .unwrap_or_default(),
    }
}

fn colors_to_wire(c: &ColorTokens) -> ThemeColors {
    ThemeColors {
        bg: c.bg.clone(),
        bg_dark: c.bg_dark.clone(),
        surface: c.surface.clone(),
        surface_raised: c.surface_raised.clone(),
        overlay: c.overlay.clone(),
        fg: c.fg.clone(),
        fg_muted: c.fg_muted.clone(),
        fg_subtle: c.fg_subtle.clone(),
        on_primary: c.on_primary.clone(),
        on_surface: c.on_surface.clone(),
        outline: c.outline.clone(),
        primary: c.primary.clone(),
        primary_variant: c.primary_variant.clone(),
        secondary: c.secondary.clone(),
        secondary_variant: c.secondary_variant.clone(),
        tertiary: c.tertiary.clone(),
        success: c.success.clone(),
        warning: c.warning.clone(),
        error: c.error.clone(),
        info: c.info.clone(),
    }
}

fn health_to_wire(h: &HealthTokens) -> ThemeHealth {
    ThemeHealth {
        stale_pill: h.stale_pill.clone(),
        error_pill: h.error_pill.clone(),
    }
}

fn bar_to_wire(b: &BarTokens) -> ThemeBar {
    ThemeBar {
        opacity: b.opacity,
        blur_radius: b.blur_radius,
        opacity_battery: b.opacity_battery,
        blur_radius_battery: b.blur_radius_battery,
        height_full: b.height_full,
        height_compact: b.height_compact,
    }
}

fn typography_to_wire(t: &TypographyTokens) -> ThemeTypography {
    ThemeTypography {
        font_text: t.font_text.clone(),
        font_mono: t.font_mono.clone(),
        font_icon: t.font_icon.clone(),
    }
}

/// Build the five colour args for Sway's `client.focused` rule
/// (`border background text indicator child_border`) from the theme's
/// palette. Returns `None` when the theme doesn't override the colours
/// we need (`primary`, `fg`, and a background) — propagating partial
/// colours would leave Sway in an inconsistent half-themed state, so we
/// skip the compositor and let it keep its config default.
pub fn sway_client_colors(c: &ColorTokens) -> Option<[String; 5]> {
    let border = c.primary.clone()?;
    let text = c.fg.clone()?;
    let background = c.bg_dark.clone().or_else(|| c.bg.clone())?;
    Some([
        border.clone(),
        background,
        text,
        border.clone(),
        border,
    ])
}

/// The `org.gnome.desktop.interface color-scheme` value for a theme
/// variant. `xdg-desktop-portal` relays this to GTK4 *and* Qt6 apps, so
/// one `gsettings` write covers both toolkits' light/dark preference.
pub fn gtk_color_scheme(variant: &str) -> Option<&'static str> {
    match variant {
        "dark" => Some("prefer-dark"),
        "light" => Some("prefer-light"),
        _ => None,
    }
}

/// Push the active theme to the compositor (Sway window colours) and
/// the GTK/Qt portal (light/dark). Fire-and-forget — each spawn is
/// detached and a missing binary is logged at debug, never propagated.
fn propagate_to_desktop(theme: &ThemeFile) {
    if let Some([b, bg, t, i, cb]) = sway_client_colors(&theme.colors) {
        if let Err(e) = crate::palette::spawn_detached(
            "swaymsg",
            &["client.focused", &b, &bg, &t, &i, &cb],
        ) {
            tracing::debug!(error = %e, "theme: swaymsg propagation skipped");
        }
    }
    if let Some(scheme) = gtk_color_scheme(&theme.meta.variant) {
        if let Err(e) = crate::palette::spawn_detached(
            "gsettings",
            &["set", "org.gnome.desktop.interface", "color-scheme", scheme],
        ) {
            tracing::debug!(error = %e, "theme: gsettings propagation skipped");
        }
    }
}

/// Handle to the themes-directory watcher. Owns the OS watch and the
/// background reload task; drop to stop watching. Mirrors
/// `levshell_config::ProfileWatcher`.
pub struct ThemeWatcher {
    _watcher: levshell_config::ConfigWatcher,
    _task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ThemeWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThemeWatcher").finish_non_exhaustive()
    }
}

/// Hot-reload (spec §3.9): when any `*.toml` under `dir` changes,
/// re-activate the current theme so editing the active theme file
/// re-pushes its payload live with no `levshell-ctl theme set`. An
/// edit to an *inactive* theme file also triggers a reload of the
/// active one — one TOML parse + payload push, cheap, and keeps the
/// watcher stateless (same policy as the profile watcher).
pub fn spawn_theme_watcher(
    dir: &Path,
    theme: std::sync::Arc<ThemeService>,
) -> Result<ThemeWatcher, levshell_config::WatcherError> {
    let (watcher, mut rx) = levshell_config::watch_config_dir(dir)?;
    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Coalesce editor write storms into one reload.
            while rx.try_recv().is_ok() {}
            match theme.reload_active() {
                Ok(()) => {
                    tracing::info!("theme hot-reload: re-applied active theme")
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "theme hot-reload: re-activate failed; keeping current"
                ),
            }
        }
        tracing::debug!("theme hot-reload: watcher channel closed");
    });
    Ok(ThemeWatcher {
        _watcher: watcher,
        _task: task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_config::{ColorTokens, ThemeMeta};

    fn make_theme(name: &str, variant: &str, light_pair: Option<&str>) -> ThemeFile {
        ThemeFile {
            meta: ThemeMeta {
                name: name.into(),
                author: None,
                variant: variant.into(),
                light_pair: light_pair.map(str::to_string),
                dark_pair: None,
            },
            colors: ColorTokens {
                primary: Some("#123456".into()),
                ..Default::default()
            },
            health: None,
            bar: None,
            typography: None,
        }
    }

    #[test]
    fn file_to_payload_passes_through_overrides() {
        let t = make_theme("Test", "dark", Some("test-light"));
        let p = file_to_payload(&t);
        assert_eq!(p.name, "Test");
        assert_eq!(p.variant, "dark");
        assert_eq!(p.light_pair.as_deref(), Some("test-light"));
        assert_eq!(p.colors.primary.as_deref(), Some("#123456"));
        assert!(p.colors.bg.is_none());
        assert_eq!(p.bar, ThemeBar::default());
    }

    #[test]
    fn activate_missing_themes_dir_errors() {
        let s = ThemeService::new(None, EventBus::new());
        let err = s.activate("warm-dark").unwrap_err();
        assert!(err.contains("no themes directory"));
    }

    #[test]
    fn activate_unknown_theme_surfaces_load_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);
        let err = s.activate("no-such-theme").unwrap_err();
        assert!(err.contains("no-such-theme") || err.contains("not found"));
    }

    #[test]
    fn activate_happy_path_publishes_bus_event() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tiny.toml"),
            r##"
[meta]
name = "Tiny"
variant = "dark"

[colors]
primary = "#7AA2F7"
"##,
        )
        .unwrap();

        let bus = EventBus::new();
        let mut rx = bus.subscribe("t", [levshell_core::EventKind::ThemeActivated], 4);
        let s = ThemeService::new(Some(dir.path().to_path_buf()), bus).with_propagation(false);
        let snap = s.activate("tiny").unwrap();
        assert_eq!(snap.name, "Tiny");
        assert_eq!(snap.variant, "dark");

        match rx.try_recv() {
            Ok(Event::ThemeActivated { name, variant }) => {
                assert_eq!(name, "Tiny");
                assert_eq!(variant, "dark");
            }
            other => panic!("expected ThemeActivated event, got {other:?}"),
        }
    }

    #[test]
    fn toggle_mode_without_pair_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("solo.toml"),
            r##"
[meta]
name = "Solo"
variant = "dark"

[colors]
"##,
        )
        .unwrap();
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);
        s.activate("solo").unwrap();
        let err = s.toggle_mode().unwrap_err();
        assert!(err.contains("light_pair") || err.contains("dark_pair"));
    }

    #[test]
    fn toggle_mode_swaps_to_paired_theme() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("paired-dark.toml"),
            r##"
[meta]
name = "Paired Dark"
variant = "dark"
light_pair = "paired-light"

[colors]
primary = "#111111"
"##,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("paired-light.toml"),
            r##"
[meta]
name = "Paired Light"
variant = "light"
dark_pair = "paired-dark"

[colors]
primary = "#EEEEEE"
"##,
        )
        .unwrap();
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);
        s.activate("paired-dark").unwrap();
        let after = s.toggle_mode().unwrap();
        assert_eq!(after.name, "Paired Light");
        assert_eq!(after.variant, "light");
    }

    #[test]
    fn toggle_mode_self_pair_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("loopy.toml"),
            r##"
[meta]
name = "Loopy"
variant = "dark"
light_pair = "loopy"

[colors]
"##,
        )
        .unwrap();
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);
        s.activate("loopy").unwrap();
        let err = s.toggle_mode().unwrap_err();
        assert!(err.contains("itself"), "got: {err}");
    }

    #[test]
    fn bundled_neutral_pair_toggles_both_ways() {
        // The shipped neutral-dark ↔ neutral-light pairing, end to end
        // through the real BUILTIN_THEMES files.
        let dir = tempfile::tempdir().unwrap();
        levshell_config::bootstrap_themes(dir.path(), false).unwrap();
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);

        let nd = s.activate("neutral-dark").unwrap();
        assert_eq!(nd.variant, "dark");
        let nl = s.toggle_mode().unwrap();
        assert_eq!(nl.name, "Neutral Light");
        assert_eq!(nl.variant, "light");
        let back = s.toggle_mode().unwrap();
        assert_eq!(back.name, "Neutral Dark");
        assert_eq!(back.variant, "dark");
    }

    #[test]
    fn sway_colors_need_primary_fg_and_bg() {
        // Only primary set → not enough to theme Sway consistently.
        let partial = ColorTokens {
            primary: Some("#7AA2F7".into()),
            ..Default::default()
        };
        assert!(sway_client_colors(&partial).is_none());

        let full = ColorTokens {
            primary: Some("#7AA2F7".into()),
            fg: Some("#C0CAF5".into()),
            bg_dark: Some("#16161E".into()),
            ..Default::default()
        };
        let got = sway_client_colors(&full).unwrap();
        // border, background, text, indicator, child_border
        assert_eq!(got[0], "#7AA2F7");
        assert_eq!(got[1], "#16161E");
        assert_eq!(got[2], "#C0CAF5");
        assert_eq!(got[3], "#7AA2F7");
        assert_eq!(got[4], "#7AA2F7");
    }

    #[test]
    fn sway_colors_fall_back_to_bg_when_no_bg_dark() {
        let c = ColorTokens {
            primary: Some("#111".into()),
            fg: Some("#eee".into()),
            bg: Some("#222".into()),
            ..Default::default()
        };
        assert_eq!(sway_client_colors(&c).unwrap()[1], "#222");
    }

    #[test]
    fn gtk_color_scheme_maps_variants() {
        assert_eq!(gtk_color_scheme("dark"), Some("prefer-dark"));
        assert_eq!(gtk_color_scheme("light"), Some("prefer-light"));
        assert_eq!(gtk_color_scheme("sepia"), None);
    }

    #[test]
    fn presentation_toggles_and_sets_explicitly() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe(
            "p",
            [levshell_core::EventKind::PresentationModeChanged],
            4,
        );
        let s = ThemeService::new(None, bus).with_propagation(false);
        assert!(!s.presentation());

        assert!(s.set_presentation(Some("on")));
        assert!(s.presentation());
        assert!(!s.set_presentation(Some("off")));
        assert!(s.set_presentation(None)); // toggle from false → true
        assert!(!s.set_presentation(Some("toggle"))); // explicit toggle flips

        // The bus saw a PresentationModeChanged for the first set.
        match rx.try_recv() {
            Ok(Event::PresentationModeChanged { on }) => assert!(on),
            other => panic!("expected PresentationModeChanged, got {other:?}"),
        }
    }

    #[test]
    fn list_returns_available_themes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::write(
                dir.path().join(format!("{name}.toml")),
                r##"
[meta]
name = "x"
variant = "dark"
[colors]
"##,
            )
            .unwrap();
        }
        let s = ThemeService::new(Some(dir.path().to_path_buf()), EventBus::new())
            .with_propagation(false);
        assert_eq!(s.list(), vec!["a".to_string(), "b".to_string()]);
    }
}
