# Levshell UI scaling — design & implementation plan

Status: **Phase 1 + Phase 2 implemented** (2026-05-18, branch
`feat/ui-scaling`). Phase 3 (hardcoded-literal sweep) still deferred.

Implemented design notes (where it diverged from the original plan):
- `config/levshell.toml` turned out to be a non-functional Phase-0 stub
  with no parser, so persistence is the committed `Theme.qml` default
  (`uiScale: 1.75`) — exactly how `density`'s default works. No new
  config subsystem was built.
- `ui_scale` is global, NOT per-theme: it flows ctl → `Event::
  UiScaleRequested` → `context_engine` (`ui.scale` signal, `cycle`
  resolution) → `DaemonMessage::UiScaleState` → shell `Binding` on
  `Theme.uiScale`. A `when: daemonUiScale > 0` guard keeps the
  `Theme.qml` default until the first push (no duplicated default, no
  flash to 1.0 on connect). This mirrors the `density` path exactly.
- `levshell-ctl scale <factor>` (range-validated 0.5..=4.0 at the clap
  layer) and `levshell-ctl scale cycle`
  (1.0→1.25→1.5→1.75→2.0→1.0). `CtlRequest::SetScale` carries the
  factor as a validated `String` so `CtlRequest: Eq` is preserved
  (the bus is stringly-typed anyway); `f64` lives only in
  `UiScaleState` (`DaemonMessage` is `PartialEq`-only).
- `statusIconSize` was scaled too (size-bearing, not in the original
  token list). `main.qml`'s TOML bar-height override now scales by
  `Theme.uiScale` so an explicit `height_full` isn't left at 1×.

No daemon-restart needed to change scale (live via ctl). A sway
keybind (`$mod+Shift+plus exec levshell-ctl scale cycle`) is left to
the user's sway config, like the density bind.

Original plan follows (kept for the Phase 3 literal table / sweep
guidance, which is still accurate and outstanding):

## Problem

Levshell has no UI scale factor. On this box `DP-2` is 3840×2160 @ sway
`scale 1.0`, so the shell renders at native 4K logical px and is too
small. Compositor-level scaling (`output … scale 2`) is **not an
option** — other installed software requires an unscaled output. So
scaling must be internal to levshell and must not depend on the
compositor.

`Theme.density` (`full`/`compact`/`hidden`) is **not** scaling — it is
information density and only drives `barHeight`/`iconSize`/`widgetPadding`/
`interWidgetGap`. Fonts, type scale, panel/overlay geometry, and spacing
tokens are unaffected by it.

## Why this is tractable

`shell/Theme.qml` is a single source of truth and widgets are
disciplined about binding to `Theme.*` tokens (e.g. `WidgetWrapper`
sizes from content + `Theme.widgetPaddingH`; text uses `Theme.typeBody`
etc.; the bar's `PanelWindow.exclusiveZone`/`implicitHeight` bind to
`Theme.barHeight`). A single multiplier applied to the size-bearing
tokens scales most of the UI from one file, and the reserved bar strip
resizes automatically.

## The catch

~70 hardcoded px literals across **16 files** bypass `Theme`. A pure
token multiplier will not touch them, so those surfaces have fixed-size
elements that stay 1× and look slightly off at 2×. Concentration
(size-bearing literals, ≥2 digits, excluding `: 0`):

| File | count |
|---|---|
| `widgets/NotificationCenter.qml` | 17 |
| `widgets/QuickSettings.qml` | 12 |
| `widgets/CommandPalette.qml` | 6 |
| `widgets/WarmupOverlay.qml` | 4 |
| `widgets/RubberDuckOverlay.qml` | 4 |
| `widgets/RemoteJobsPanel.qml` | 4 |
| `widgets/ProjectPulsePanel.qml` | 4 |
| `widgets/ProcessSniper.qml` | 4 |
| `widgets/ClockHub.qml` | 4 |
| `widgets/SshFleetPanel.qml` | 3 |
| `widgets/ReferenceLibraryPanel.qml` | 2 |
| `widgets/GpuFleetPanel.qml` | 2 |
| `widgets/{Sparkline,MemoryWidget,CpuWidget,…}.qml` | 1 each |

(Regenerate the exact list with:
`grep -rnE '(implicitWidth|implicitHeight|width|height|pixelSize|spacing|radius): *[0-9]{2,}' shell/widgets/*.qml shell/main.qml | grep -vE 'Theme\.|: 0\b'`)

## Implementation phases

### Phase 1 — token multiplier (core, ~1 file)

In `shell/Theme.qml`:

1. Add `property real uiScale: 1.0` near the top of the singleton
   (above the type scale block, ~line 110).
2. Convert the size-bearing **readonly** tokens from literals to
   `Math.round(<base> * uiScale)`. Scope:
   - Type scale: `typeDisplay`, `typeHeadline`, `typeTitle`, `typeBody`,
     `typeBodyEmphasisSize`, `typeLabel`, `typeCaption` (lines ~114–127).
     Do **not** scale the `*Weight` tokens.
   - Spacing: `spaceXs … space2xl` (lines ~137–142).
   - Density token pairs: `barHeightFull/Compact`, `iconSizeFull/Compact`,
     `widgetPaddingH/V Full/Compact`, `interWidgetGap Full/Compact`
     (lines ~183–196). Leave `barHeightHidden`/`widthHidden` at 0.
   - Panel/overlay geometry: `panelCornerRadius`, `panelInnerPadding`,
     `panelBorderWidth`, `panelShadowOffsetY`, `panelShadowBlur`
     (lines ~216–225), `widthBadge` (~267).
   - **Do not** scale: motion durations, spring constants/damping,
     opacities, `*Weight`, `widgetCornerRadius` (0), icon codepoints.
3. Keep the computed density accessors (`barHeight`, `iconSize`, …)
   as-is — they derive from the now-scaled pairs automatically.

Edge checks:
- `Math.round` avoids subpixel blur on borders/1px strips; verify
  `panelBorderWidth*scale` stays ≥1.
- Confirm nothing reads a raw `*Full`/`*Compact` literal expecting an
  unscaled value (grep usages).

This alone gives a clean ~90% 2×; the 16 files above are the residual.

### Phase 2 — wiring (match the existing `density` pattern)

`density` is daemon-driven via `context_engine` → `bar.density` signal →
shell. Mirror it for scale:

- Config: add `[appearance] ui_scale = 2.0` to the theme TOML
  (`config/` templates + `~/.config/levshell/`). Parse in the theme/
  config loader alongside `[typography]` (`Theme.qml:103–110` notes the
  TOML override hook; daemon side in `levshell-modules::theme` /
  `levshell-config`).
- ctl: add `levshell-ctl scale <factor>` (and/or a `scale cycle`
  1.0→1.5→2.0), additive `CtlRequest::SetScale` variant, resolved
  server-side and pushed down the same signal path that carries
  `bar.density`. Reference: the `DensityCycle` precedent
  (`levshell-ctl density cycle`, resolved in `context_engine`).
- Shell: bind `Theme.uiScale` to the pushed value (same mechanism as
  `shell.daemonDensity` → `Theme.density`).
- Optional: a sway keybind (`$mod+Shift+=` / `-`) like the density bind.

Stopgap if Phase 2 is deferred: hardcode `uiScale: 2.0` in `Theme.qml`
to get a usable 2× on this box immediately (no config/ctl).

### Phase 3 — hardcoded-literal sweep (full clean, ~17 files)

For each file in the table: replace size literals with the nearest
`Theme.*` token, or `Math.round(n * Theme.uiScale)` where no token
fits. Prioritize the heavy four (`NotificationCenter`, `QuickSettings`,
`CommandPalette`, overlays). Needs a visual pass at 1× and 2× per file.
Leave genuinely scale-invariant values (z, 1px hairlines that should
stay 1px, durations) alone.

## Verification (every phase)

- `cargo build --release --workspace` (only needed if Phase 2 touches
  Rust); QML-only phases need just a shell reload.
- Safe-restart procedure (kill by explicit PID, separate relaunch,
  `levshell-ctl status` → `shell_connected`, check `shell.log` has no
  `is not a type` / load error after `Configuration Loaded`).
- Visual check at `uiScale` 1.0 and 2.0: bar height + exclusive zone,
  every overlay (palette, notification center, quick settings, the M3/M4
  panels), text legibility, no clipped/overlapping content, 1px borders
  still crisp.
- Fractional scales (1.25, 1.5) if Phase 2 exposes arbitrary factors —
  watch for `Math.round` collapsing small tokens (e.g. `spaceXs=2`).

## Recommended path

Phase 1 + Phase 2 (defer Phase 3 as a follow-up): gives a properly
configurable scale that's ~90% clean immediately, with the residual
literal-heavy panels tracked for a later sweep. Phase 1 alone (hardcoded
`uiScale: 2.0`) is a valid same-day stopgap to validate it on this box.

## Risks / unknowns

- A few overlays may have implicit min-sizes assuming 1× content;
  check `CommandPalette`/`NotificationCenter` don't clip at 2×.
- Phosphor icon glyphs scale via font `pixelSize` (driven by
  `iconSize`/type tokens) — fine, but re-verify the previously
  codepoint-churned glyphs render at 2×.
- `Math.round` on already-small tokens at fractional scales can
  collapse distinctions (`spaceXs`/`spaceSm`); acceptable at 2×.
</content>
</invoke>
