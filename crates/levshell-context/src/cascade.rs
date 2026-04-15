//! Five-step prominence cascade (spec §3.5.4).
//!
//! ```text
//! 1. Base layout:        seed each widget with its default_prominence.
//! 2. Module relevance:   apply rules whose predicate matches the current
//!                        signal context. Multiple hits per widget keep
//!                        the *highest* prominence (spec §3.5.4 says
//!                        modules "vote" for their preferred prominence,
//!                        and the most insistent vote wins).
//! 3. Active profile:     profile overrides replace relevance-layer
//!                        decisions unconditionally for mentioned widgets.
//! 4. Spatial constraint: walk widgets in priority order and demote any
//!                        that don't fit the available pixel budget,
//!                        stepping down one prominence level at a time.
//! 5. User override:      pinned/hidden decisions override everything.
//! ```
//!
//! The function is a pure mapping from inputs to outputs. Given the same
//! inputs it always produces the same outputs, which lets the unit tests
//! exercise it without any time or async machinery.

use std::collections::HashMap;

use crate::rules::{CompiledRule, Profile, WidgetDef, Zone};
use crate::signals::SignalContext;
use levshell_ipc::{BarLayout, Prominence};

/// Signature for a function that estimates the pixel width of `widget_id`
/// at `prominence`. Supplied by the caller so the cascade stays pure.
///
/// Phase 1.2 uses a constant heuristic table. Phase 1.4 will replace it
/// with real QML-measured widths pushed from the shell.
pub type WidgetWidthFn = dyn Fn(&str, Prominence) -> u32;

/// Everything the cascade needs to produce a [`Layout`]. Borrowed from
/// the caller; no ownership changes hands.
pub struct CascadeInput<'a> {
    pub widgets: &'a [WidgetDef],
    pub signals: &'a SignalContext,
    pub rules: &'a [CompiledRule],
    pub active_profile: Option<&'a Profile>,
    /// The bar's pixel budget. Widgets exceeding this get demoted.
    pub available_width: u32,
    /// Pixel estimator (see [`WidgetWidthFn`]).
    pub widget_width: &'a WidgetWidthFn,
    /// User-pinned prominence overrides. Applied after spatial constraints
    /// and always win.
    pub user_overrides: &'a HashMap<String, Prominence>,
    /// Inter-widget gap in pixels. Added per-pair when summing widths.
    pub inter_widget_gap: u32,
}

/// Result of a cascade evaluation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    /// Final prominence per widget. Widgets at `Hidden` are kept in the
    /// map so callers can still emit a WidgetVisibility message with the
    /// hidden state.
    pub prominences: HashMap<String, Prominence>,
    /// Display ordering split across the three bar zones. Widget IDs are
    /// ordered by ascending priority within each zone so stable layouts
    /// are cheap to render.
    pub layout: BarLayout,
}

/// Run the five-step cascade. See module docs for the step definitions.
pub fn resolve_layout(input: CascadeInput) -> Layout {
    // Step 1: seed with defaults.
    let mut prominences: HashMap<String, Prominence> = input
        .widgets
        .iter()
        .map(|w| (w.id.clone(), w.default_prominence))
        .collect();

    // Step 2: module relevance rules. Highest prominence wins per widget.
    for rule in input.rules {
        if !input.widgets.iter().any(|w| w.id == rule.target) {
            // Rule targets a widget the engine doesn't know about. Skip
            // silently — this can happen when a module is unregistered
            // but its rules still float around in config.
            continue;
        }
        if !rule.evaluate(input.signals) {
            continue;
        }
        let slot = prominences.entry(rule.target.clone()).or_insert(Prominence::Hidden);
        if rule.prominence > *slot {
            *slot = rule.prominence;
        }
    }

    // Step 3: active profile overrides. Unconditional replacement for
    // mentioned widgets.
    if let Some(profile) = input.active_profile {
        for (widget, prom) in &profile.overrides {
            if let Some(slot) = prominences.get_mut(widget) {
                *slot = *prom;
            }
        }
    }

    // Step 4: spatial constraint demotion. Walk widgets in priority order
    // (descending — highest priority gets placed first). For each widget,
    // try to place it at its current prominence; if adding its width would
    // exceed the available budget, demote one step and retry. A widget
    // demoted to Hidden occupies zero width and contributes no gap.
    //
    // This matches spec §3.5.4 step 4: high-priority widgets keep their
    // resolved prominence; low-priority widgets are the ones that shrink
    // or disappear when space runs out.
    let mut widgets_by_priority: Vec<&WidgetDef> = input.widgets.iter().collect();
    widgets_by_priority.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));

    let mut running: u32 = 0;
    let mut visible_count: u32 = 0;
    for widget in &widgets_by_priority {
        loop {
            let p = *prominences
                .get(&widget.id)
                .expect("widget seeded in step 1");
            if p == Prominence::Hidden {
                break;
            }
            let w = (input.widget_width)(&widget.id, p);
            let gap = if visible_count > 0 {
                input.inter_widget_gap
            } else {
                0
            };
            if running.saturating_add(w).saturating_add(gap) <= input.available_width {
                running = running.saturating_add(w).saturating_add(gap);
                visible_count += 1;
                break;
            }
            // Doesn't fit at this prominence — demote one step and retry.
            let demoted = demote_one(p);
            prominences.insert(widget.id.clone(), demoted);
            if demoted == Prominence::Hidden {
                // Reached the floor; stop retrying this widget.
                break;
            }
        }
    }

    // Step 5: user overrides. Absolute.
    for (widget, prom) in input.user_overrides {
        if let Some(slot) = prominences.get_mut(widget) {
            *slot = *prom;
        }
    }

    // Build the zone layout. Only include widgets that are not Hidden.
    let mut left: Vec<(i32, String)> = Vec::new();
    let mut center: Vec<(i32, String)> = Vec::new();
    let mut right: Vec<(i32, String)> = Vec::new();
    for w in input.widgets {
        let p = prominences.get(&w.id).copied().unwrap_or(Prominence::Hidden);
        if p == Prominence::Hidden {
            continue;
        }
        match w.zone {
            Zone::Left => left.push((w.priority, w.id.clone())),
            Zone::Center => center.push((w.priority, w.id.clone())),
            Zone::Right => right.push((w.priority, w.id.clone())),
        }
    }
    // Descending priority within each zone, with id as a stable tiebreaker.
    for v in [&mut left, &mut center, &mut right] {
        v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    }

    Layout {
        prominences,
        layout: BarLayout {
            left: left.into_iter().map(|(_, id)| id).collect(),
            center: center.into_iter().map(|(_, id)| id).collect(),
            right: right.into_iter().map(|(_, id)| id).collect(),
        },
    }
}

fn demote_one(p: Prominence) -> Prominence {
    match p {
        Prominence::Expanded => Prominence::Visible,
        Prominence::Visible => Prominence::Compact,
        Prominence::Compact => Prominence::IconOnly,
        Prominence::IconOnly => Prominence::Badge,
        Prominence::Badge => Prominence::Hidden,
        Prominence::Hidden => Prominence::Hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse_expression;

    fn heuristic_width(_id: &str, p: Prominence) -> u32 {
        match p {
            Prominence::Hidden => 0,
            Prominence::Badge => 16,
            Prominence::IconOnly => 32,
            Prominence::Compact => 80,
            Prominence::Visible => 140,
            Prominence::Expanded => 140,
        }
    }

    fn widgets_basic() -> Vec<WidgetDef> {
        vec![
            WidgetDef::new("clock", "clock")
                .with_default(Prominence::Visible)
                .with_priority(100)
                .with_zone(Zone::Center),
            WidgetDef::new("cpu", "telemetry")
                .with_default(Prominence::Compact)
                .with_priority(40)
                .with_zone(Zone::Right),
            WidgetDef::new("battery", "telemetry")
                .with_default(Prominence::IconOnly)
                .with_priority(60)
                .with_zone(Zone::Right),
            WidgetDef::new("notifications", "notifications")
                .with_default(Prominence::IconOnly)
                .with_priority(20)
                .with_zone(Zone::Right),
        ]
    }

    fn input<'a>(
        widgets: &'a [WidgetDef],
        signals: &'a SignalContext,
        rules: &'a [CompiledRule],
        profile: Option<&'a Profile>,
        width: u32,
        overrides: &'a HashMap<String, Prominence>,
    ) -> CascadeInput<'a> {
        CascadeInput {
            widgets,
            signals,
            rules,
            active_profile: profile,
            available_width: width,
            widget_width: &heuristic_width,
            user_overrides: overrides,
            inter_widget_gap: 8,
        }
    }

    #[test]
    fn step1_seeds_from_defaults_when_nothing_else_applies() {
        let widgets = widgets_basic();
        let signals = SignalContext::new();
        let rules: Vec<CompiledRule> = Vec::new();
        let overrides = HashMap::new();
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 2000, &overrides));

        assert_eq!(out.prominences.get("clock"), Some(&Prominence::Visible));
        assert_eq!(out.prominences.get("cpu"), Some(&Prominence::Compact));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::IconOnly));
    }

    #[test]
    fn step2_rule_promotes_widget_when_signal_matches() {
        let widgets = widgets_basic();
        let signals = SignalContext::new().with("battery.percent", 12.0_f64);
        let rules = vec![CompiledRule::new(
            "battery",
            parse_expression("battery.percent < 20").unwrap(),
            Prominence::Visible,
        )];
        let overrides = HashMap::new();
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 2000, &overrides));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::Visible));
    }

    #[test]
    fn step2_highest_rule_prominence_wins_per_widget() {
        let widgets = widgets_basic();
        let signals = SignalContext::new()
            .with("battery.percent", 12.0_f64)
            .with("power.on_battery", true);
        let rules = vec![
            CompiledRule::new(
                "battery",
                parse_expression("battery.percent < 20").unwrap(),
                Prominence::Compact,
            ),
            CompiledRule::new(
                "battery",
                parse_expression("power.on_battery").unwrap(),
                Prominence::Expanded,
            ),
        ];
        let overrides = HashMap::new();
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 2000, &overrides));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::Expanded));
    }

    #[test]
    fn step3_profile_override_replaces_relevance_decision() {
        let widgets = widgets_basic();
        let signals = SignalContext::new().with("battery.percent", 12.0_f64);
        let rules = vec![CompiledRule::new(
            "battery",
            parse_expression("battery.percent < 20").unwrap(),
            Prominence::Expanded,
        )];
        let profile = Profile::new("writing").with("battery", Prominence::Badge);
        let overrides = HashMap::new();
        let out = resolve_layout(input(
            &widgets,
            &signals,
            &rules,
            Some(&profile),
            2000,
            &overrides,
        ));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::Badge));
    }

    #[test]
    fn step4_narrow_bar_demotes_lowest_priority_widgets_first() {
        let widgets = widgets_basic();
        let signals = SignalContext::new();
        let rules: Vec<CompiledRule> = Vec::new();
        let overrides = HashMap::new();

        // Placement walks descending priority: clock(100), battery(60),
        // cpu(40), notifications(20). Budget is 244px.
        //
        //   clock    V 140   → running=140
        //   battery  I 32    → running=180 (gap +8)
        //   cpu      C 80    → 180+80+8=268 > 244. Demote to I(32).
        //                       180+32+8=220. Place. cpu=IconOnly.
        //   notif    I 32    → 220+32+8=260 > 244. Demote to B(16).
        //                       220+16+8=244. Place. notif=Badge.
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 244, &overrides));

        assert_eq!(out.prominences.get("clock"), Some(&Prominence::Visible));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::IconOnly));
        assert_eq!(out.prominences.get("cpu"), Some(&Prominence::IconOnly));
        assert_eq!(out.prominences.get("notifications"), Some(&Prominence::Badge));
    }

    #[test]
    fn step4_very_narrow_bar_hides_lowest_priority_widgets() {
        let widgets = widgets_basic();
        let signals = SignalContext::new();
        let rules: Vec<CompiledRule> = Vec::new();
        let overrides = HashMap::new();

        // Budget=180 only fits the clock (140) plus one more IconOnly
        // (32) with gap (8) = 180. Remaining low-priority widgets should
        // land at Hidden.
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 180, &overrides));

        assert_eq!(out.prominences.get("clock"), Some(&Prominence::Visible));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::IconOnly));
        assert_eq!(out.prominences.get("cpu"), Some(&Prominence::Hidden));
        assert_eq!(out.prominences.get("notifications"), Some(&Prominence::Hidden));
    }

    #[test]
    fn step5_user_override_wins_over_all_prior_steps() {
        let widgets = widgets_basic();
        let signals = SignalContext::new().with("battery.percent", 12.0_f64);
        let rules = vec![CompiledRule::new(
            "battery",
            parse_expression("battery.percent < 20").unwrap(),
            Prominence::Expanded,
        )];
        let profile = Profile::new("writing").with("battery", Prominence::Badge);
        let mut overrides = HashMap::new();
        overrides.insert("battery".to_string(), Prominence::Hidden);

        let out = resolve_layout(input(
            &widgets,
            &signals,
            &rules,
            Some(&profile),
            2000,
            &overrides,
        ));
        assert_eq!(out.prominences.get("battery"), Some(&Prominence::Hidden));
    }

    #[test]
    fn zones_are_sorted_by_descending_priority() {
        let widgets = widgets_basic();
        let signals = SignalContext::new();
        let rules: Vec<CompiledRule> = Vec::new();
        let overrides = HashMap::new();
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 2000, &overrides));
        // Right zone has battery(60) > cpu(40) > notifications(20).
        assert_eq!(out.layout.right, vec!["battery", "cpu", "notifications"]);
        assert_eq!(out.layout.center, vec!["clock"]);
        assert!(out.layout.left.is_empty());
    }

    #[test]
    fn hidden_widgets_are_excluded_from_layout_zones() {
        let widgets = widgets_basic();
        let signals = SignalContext::new();
        let rules: Vec<CompiledRule> = Vec::new();
        let mut overrides = HashMap::new();
        overrides.insert("cpu".to_string(), Prominence::Hidden);
        let out = resolve_layout(input(&widgets, &signals, &rules, None, 2000, &overrides));
        assert!(!out.layout.right.iter().any(|id| id == "cpu"));
    }

    #[test]
    fn deterministic_given_same_inputs() {
        let widgets = widgets_basic();
        let signals = SignalContext::new().with("battery.percent", 12.0_f64);
        let rules = vec![CompiledRule::new(
            "battery",
            parse_expression("battery.percent < 20").unwrap(),
            Prominence::Visible,
        )];
        let overrides = HashMap::new();
        let a = resolve_layout(input(&widgets, &signals, &rules, None, 400, &overrides));
        let b = resolve_layout(input(&widgets, &signals, &rules, None, 400, &overrides));
        assert_eq!(a, b);
    }
}
