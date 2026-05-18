//! Candidate picker for the ideation engine (spec §2.2).
//!
//! The selector is intentionally stateless and pure: given a
//! [`NudgeContext`] snapshot and an `rng`, it returns at most one
//! [`Nudge`]. That lets the tick-driven module thread all the async
//! fetching, and lets tests drive the selector with a fixed RNG and
//! handcrafted snapshots.
//!
//! ## Algorithm
//!
//! 1. Compute normalized weights for the three kinds
//!    (`OpenQuestion`, `CrossConnection`, `BlockedEscalation`).
//!    Weights of zero are skipped entirely.
//! 2. Roll a weighted choice. If the chosen kind has at least one
//!    candidate, pick one uniformly and emit.
//! 3. Otherwise, fall through to the remaining kinds in descending
//!    weight order until one produces a candidate or we exhaust them.
//!
//! Cross-connection has two bases, tried in confidence order:
//!
//! 1. **Graph-mined** (preferred): the entity has a relation-graph
//!    edge (`wiki_link`, `scaffolded_from`, …) to a neighbour that
//!    lives in a *different* active project. The user has literally
//!    drawn this link across a project boundary — a latent
//!    cross-pollination they may not have noticed at the project
//!    level. High confidence: it's a real edge, not a guess.
//! 2. **Tag-overlap** (fallback): a recently-synced entity's tags
//!    overlap with a project *other than* the entity's owner. A
//!    weaker thematic hint, used only when no graph edge connects the
//!    entity across projects (sparse graphs / new libraries).
//!
//! Either way the nudge targets the *other* project so we never nudge
//! about a link the user has already surfaced there.

use chrono::{DateTime, Utc};
use levshell_data::{EntityType, ProjectStatus};
use levshell_projects::ProjectEntry;
use rand::seq::SliceRandom;
use rand::Rng;
use uuid::Uuid;

use super::config::IdeationConfig;
use super::nudge::{Nudge, NudgeKind};

/// Snapshot handed to [`NudgeSelector::select`]. Owned by the caller;
/// the selector borrows from it without copying.
#[derive(Debug, Clone)]
pub struct NudgeContext<'a> {
    pub projects: &'a [ProjectEntry],
    pub recent_entities: &'a [RecentEntity],
    pub now: DateTime<Utc>,
    pub config: &'a IdeationConfig,
}

/// A recently-synced or recently-edited Note or Reference, enriched
/// with its tag set. The ideation engine looks at these to find
/// cross-project connections (spec §2.2.2).
#[derive(Debug, Clone)]
pub struct RecentEntity {
    pub id: Uuid,
    pub entity_type: EntityType,
    pub title: String,
    /// Project the entity already belongs to (if any). Cross-connection
    /// nudges explicitly pick a *different* project so we don't nudge
    /// the user about a link they've already drawn.
    pub project_id: Option<Uuid>,
    pub tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
    /// Relation-graph neighbours of this entity, each carrying the
    /// neighbour's owning project (if any). Populated by the module
    /// from `DataStore::related_entities`; the graph-mined
    /// cross-connection pass looks for a neighbour in a *different*
    /// project. Empty when the entity has no edges (the common case
    /// for a fresh library) — the selector then falls back to tags.
    pub graph_links: Vec<GraphLink>,
}

/// One relation-graph edge from a [`RecentEntity`] to a neighbour,
/// pre-resolved by the module so the selector stays pure (no async,
/// no store).
#[derive(Debug, Clone)]
pub struct GraphLink {
    /// `entity_relations.relation_kind` verbatim (`wiki_link`,
    /// `scaffolded_from`, …) — used in the nudge body so the user
    /// knows *how* the two are connected.
    pub kind: String,
    /// Human label for the neighbour (note/ref title, `@citekey …`).
    pub neighbour_label: String,
    /// The neighbour's owning project, if it belongs to one. `None`
    /// neighbours can't be a *cross*-project signal and are skipped.
    pub neighbour_project: Option<Uuid>,
}

#[derive(Debug, Default, Clone)]
pub struct NudgeSelector;

impl NudgeSelector {
    pub fn new() -> Self {
        Self
    }

    pub fn select<R: Rng>(&self, ctx: &NudgeContext<'_>, rng: &mut R) -> Option<Nudge> {
        // Build a ranked list of (kind, weight) entries. Ranked by
        // weight descending so the fall-through order is deterministic
        // when multiple kinds tie.
        let weights = &ctx.config.weights;
        let mut ranked: Vec<(NudgeKind, f64)> = vec![
            (NudgeKind::OpenQuestion, weights.open_question),
            (NudgeKind::CrossConnection, weights.cross_connection),
            (NudgeKind::BlockedEscalation, weights.blocked),
        ];
        ranked.retain(|(_, w)| *w > 0.0);
        if ranked.is_empty() {
            return None;
        }
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let total: f64 = ranked.iter().map(|(_, w)| w).sum();
        let mut roll = rng.gen::<f64>() * total;
        let mut first = None;
        for (kind, w) in &ranked {
            roll -= *w;
            if roll <= 0.0 {
                first = Some(*kind);
                break;
            }
        }
        let first = first.unwrap_or(ranked[0].0);

        // Try the picked kind first, then fall through the remaining
        // kinds in ranked order until one produces a candidate.
        let attempt_order: Vec<NudgeKind> = std::iter::once(first)
            .chain(ranked.iter().map(|(k, _)| *k).filter(|k| *k != first))
            .collect();

        for kind in attempt_order {
            let maybe = match kind {
                NudgeKind::OpenQuestion => pick_open_question(ctx, rng),
                NudgeKind::CrossConnection => pick_cross_connection(ctx, rng),
                NudgeKind::BlockedEscalation => pick_blocked_escalation(ctx, rng),
            };
            if maybe.is_some() {
                return maybe;
            }
        }
        None
    }
}

fn eligible_projects<'a>(ctx: &'a NudgeContext<'a>) -> Vec<&'a ProjectEntry> {
    ctx.projects
        .iter()
        .filter(|p| !matches!(p.project.status, ProjectStatus::Complete))
        .collect()
}

fn pick_open_question<R: Rng>(ctx: &NudgeContext<'_>, rng: &mut R) -> Option<Nudge> {
    let candidates: Vec<(&ProjectEntry, &String)> = eligible_projects(ctx)
        .into_iter()
        .flat_map(|p| {
            p.project
                .open_questions
                .iter()
                .map(move |q| (p, q))
        })
        .collect();
    let (project, question) = candidates.choose(rng)?;
    Some(Nudge {
        project_id: project.project.id,
        kind: NudgeKind::OpenQuestion,
        title: project.project.name.clone(),
        body: (*question).clone(),
    })
}

fn entity_label(ty: EntityType) -> &'static str {
    match ty {
        EntityType::Reference => "reference",
        _ => "note",
    }
}

fn pick_cross_connection<R: Rng>(ctx: &NudgeContext<'_>, rng: &mut R) -> Option<Nudge> {
    // Only consider entities inside the configured recency window.
    // (No tag requirement here — the graph-mined pass doesn't need
    // tags; the tag pass filters per-entity below.)
    let horizon = ctx.now
        - chrono::Duration::hours(ctx.config.recent_seed_hours.max(1) as i64);
    let fresh: Vec<&RecentEntity> = ctx
        .recent_entities
        .iter()
        .filter(|e| e.updated_at >= horizon)
        .collect();
    if fresh.is_empty() {
        return None;
    }

    // Shuffle so ties don't always pick the first entity. One shuffle
    // shared by both passes; an empty `choose` consumes no RNG, so the
    // graph pass not firing leaves the tag pass's draw order unchanged.
    let mut shuffled = fresh;
    shuffled.shuffle(rng);
    let eligible = eligible_projects(ctx);

    // Pass 1 — graph-mined (high confidence). A real relation-graph
    // edge whose neighbour lives in a *different* active project: the
    // user has already drawn this link across a project boundary.
    for entity in &shuffled {
        let candidates: Vec<(&ProjectEntry, &GraphLink)> = entity
            .graph_links
            .iter()
            .filter_map(|link| {
                let np = link.neighbour_project?;
                // Same project as the entity → not a *cross* link.
                if Some(np) == entity.project_id {
                    return None;
                }
                let project = eligible.iter().find(|p| p.project.id == np)?;
                Some((*project, link))
            })
            .collect();

        if let Some((project, link)) = candidates.choose(rng) {
            return Some(Nudge {
                project_id: project.project.id,
                kind: NudgeKind::CrossConnection,
                title: project.project.name.clone(),
                body: format!(
                    "Recent {} \"{}\" is linked ({}) to \"{}\" in this project — surface the connection?",
                    entity_label(entity.entity_type),
                    entity.title,
                    link.kind,
                    link.neighbour_label
                ),
            });
        }
    }

    // Pass 2 — tag-overlap fallback. A thematic hint when no edge
    // crosses a project boundary yet.
    for entity in &shuffled {
        if entity.tags.is_empty() {
            continue;
        }
        // Projects the entity does NOT already belong to, with at
        // least one overlapping tag.
        let overlaps: Vec<(&ProjectEntry, Vec<String>)> = eligible
            .iter()
            .filter(|p| entity.project_id != Some(p.project.id))
            .filter_map(|p| {
                let common: Vec<String> = p
                    .metadata
                    .tags
                    .iter()
                    .filter(|t| entity.tags.iter().any(|et| et == *t))
                    .cloned()
                    .collect();
                if common.is_empty() {
                    None
                } else {
                    Some((*p, common))
                }
            })
            .collect();

        if let Some((project, common)) = overlaps.choose(rng) {
            return Some(Nudge {
                project_id: project.project.id,
                kind: NudgeKind::CrossConnection,
                title: project.project.name.clone(),
                body: format!(
                    "Recent {} \"{}\" shares tags [{}] with this project.",
                    entity_label(entity.entity_type),
                    entity.title,
                    common.join(", ")
                ),
            });
        }
    }
    None
}

fn pick_blocked_escalation<R: Rng>(ctx: &NudgeContext<'_>, rng: &mut R) -> Option<Nudge> {
    let stale_cutoff = ctx.now
        - chrono::Duration::hours(ctx.config.stale_project_hours.max(1) as i64);

    let candidates: Vec<&ProjectEntry> = eligible_projects(ctx)
        .into_iter()
        .filter(|p| {
            matches!(p.project.status, ProjectStatus::Blocked)
                || p.project.updated_at < stale_cutoff
        })
        .collect();
    let project = *candidates.choose(rng)?;

    let body = if matches!(project.project.status, ProjectStatus::Blocked) {
        "This project is marked blocked. What's the smallest concrete next step you could take right now?".to_string()
    } else {
        format!(
            "This project has gone quiet since {}. What's the smallest concrete next step you could take right now?",
            project.project.updated_at.format("%Y-%m-%d")
        )
    };
    Some(Nudge {
        project_id: project.project.id,
        kind: NudgeKind::BlockedEscalation,
        title: project.project.name.clone(),
        body,
    })
}

/// Whether the ideation engine should fire a nudge this tick.
///
/// Rolls a Bernoulli trial with probability `base × (blocked_factor if
/// any blocked project exists else 1.0)`. The caller is responsible
/// for not passing an rng between threads without synchronization.
pub fn should_fire_this_tick<R: Rng>(
    ctx: &NudgeContext<'_>,
    rng: &mut R,
) -> bool {
    let mut p = ctx.config.base_fire_probability();
    let any_blocked = ctx
        .projects
        .iter()
        .any(|p| matches!(p.project.status, ProjectStatus::Blocked));
    if any_blocked {
        p *= ctx.config.blocked_escalation_factor.max(0.0);
    }
    p = p.clamp(0.0, 1.0);
    rng.gen::<f64>() < p
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use levshell_data::Project;
    use levshell_projects::{ProjectMetadata, ProjectRuntime};
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::BTreeSet;

    fn entry(
        id: Uuid,
        name: &str,
        status: ProjectStatus,
        open_questions: Vec<String>,
        tags: Vec<String>,
        updated_at: DateTime<Utc>,
    ) -> ProjectEntry {
        ProjectEntry {
            project: Project {
                id,
                name: name.into(),
                status,
                description: String::new(),
                open_questions,
                created_at: updated_at,
                updated_at,
            },
            metadata: ProjectMetadata {
                tags,
                git_repos: vec![],
                ssh_hosts: vec![],
                workspace_names: vec![],
                accent_color: None,
            },
            runtime: ProjectRuntime {
                last_active_at: None,
                accumulated_focus_time_secs: 0,
                currently_active_workspaces: BTreeSet::new(),
            },
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 16, 12, 0, 0).unwrap()
    }

    fn base_config() -> IdeationConfig {
        IdeationConfig::default()
    }

    #[test]
    fn empty_registry_produces_no_nudge() {
        let ctx = NudgeContext {
            projects: &[],
            recent_entities: &[],
            now: fixed_now(),
            config: &base_config(),
        };
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(NudgeSelector::new().select(&ctx, &mut rng), None);
    }

    #[test]
    fn open_question_kind_is_picked_when_available() {
        let project_id = Uuid::now_v7();
        let projects = vec![entry(
            project_id,
            "Levshell",
            ProjectStatus::Active,
            vec!["What blocks density morphing?".into()],
            vec![],
            fixed_now(),
        )];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 1.0,
                    cross_connection: 0.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(42);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::OpenQuestion);
        assert_eq!(nudge.project_id, project_id);
        assert_eq!(nudge.body, "What blocks density morphing?");
    }

    #[test]
    fn blocked_escalation_fires_on_blocked_projects() {
        let blocked_id = Uuid::now_v7();
        let active_id = Uuid::now_v7();
        let projects = vec![
            entry(
                blocked_id,
                "Stuck",
                ProjectStatus::Blocked,
                vec![],
                vec![],
                fixed_now(),
            ),
            entry(
                active_id,
                "Running",
                ProjectStatus::Active,
                vec![],
                vec![],
                fixed_now(),
            ),
        ];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 0.0,
                    blocked: 1.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(7);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::BlockedEscalation);
        assert_eq!(nudge.project_id, blocked_id);
    }

    #[test]
    fn stale_active_project_escalates_without_blocked_status() {
        let stale_id = Uuid::now_v7();
        let recent_cutoff = fixed_now() - chrono::Duration::hours(48);
        let projects = vec![entry(
            stale_id,
            "Quiet",
            ProjectStatus::Active,
            vec![],
            vec![],
            recent_cutoff,
        )];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 0.0,
                    blocked: 1.0,
                },
                stale_project_hours: 24,
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(3);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::BlockedEscalation);
    }

    #[test]
    fn completed_projects_are_ignored() {
        let completed_id = Uuid::now_v7();
        let projects = vec![entry(
            completed_id,
            "Done",
            ProjectStatus::Complete,
            vec!["never asked".into()],
            vec![],
            fixed_now(),
        )];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 1.0,
                    cross_connection: 0.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(5);
        assert_eq!(NudgeSelector::new().select(&ctx, &mut rng), None);
    }

    #[test]
    fn cross_connection_requires_tag_overlap_with_different_project() {
        let project_a = Uuid::now_v7();
        let project_b = Uuid::now_v7();
        let note_id = Uuid::now_v7();
        let projects = vec![
            entry(
                project_a,
                "Shell",
                ProjectStatus::Active,
                vec![],
                vec!["qml".into(), "shell".into()],
                fixed_now(),
            ),
            entry(
                project_b,
                "Daemon",
                ProjectStatus::Active,
                vec![],
                vec!["rust".into(), "shell".into()],
                fixed_now(),
            ),
        ];
        let recent = vec![RecentEntity {
            id: note_id,
            entity_type: EntityType::Note,
            title: "Bar widgets".into(),
            project_id: Some(project_a), // already owned by A
            tags: vec!["shell".into(), "qml".into()],
            updated_at: fixed_now(),
            graph_links: vec![],
        }];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &recent,
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 1.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(11);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::CrossConnection);
        assert_eq!(
            nudge.project_id, project_b,
            "must pick a project other than the one the entity already belongs to"
        );
        assert!(nudge.body.contains("shell"));
    }

    #[test]
    fn graph_mined_link_beats_tag_overlap_and_names_the_edge() {
        let project_a = Uuid::now_v7();
        let project_b = Uuid::now_v7();
        let note_id = Uuid::now_v7();
        let projects = vec![
            entry(
                project_a,
                "Reading",
                ProjectStatus::Active,
                vec![],
                vec![], // no tags: the only basis is the graph edge
                fixed_now(),
            ),
            entry(
                project_b,
                "Thesis",
                ProjectStatus::Active,
                vec![],
                vec![],
                fixed_now(),
            ),
        ];
        let recent = vec![RecentEntity {
            id: note_id,
            entity_type: EntityType::Note,
            title: "Sparse attention scratch".into(),
            project_id: Some(project_a), // note lives in A …
            tags: vec![],
            updated_at: fixed_now(),
            // … but it wiki-links a note that lives in B.
            graph_links: vec![GraphLink {
                kind: "wiki_link".into(),
                neighbour_label: "Longformer summary".into(),
                neighbour_project: Some(project_b),
            }],
        }];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &recent,
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 1.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(23);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::CrossConnection);
        assert_eq!(
            nudge.project_id, project_b,
            "nudge targets the neighbour's project, not the entity's own"
        );
        assert!(
            nudge.body.contains("wiki_link"),
            "body names how they're connected: {}",
            nudge.body
        );
        assert!(
            nudge.body.contains("Longformer summary"),
            "body names the linked neighbour: {}",
            nudge.body
        );
    }

    #[test]
    fn graph_link_within_same_project_is_not_cross_connection() {
        // A note links to another note in its *own* project — already
        // surfaced there, so no cross-connection. With no tags and no
        // other basis, the selector emits nothing.
        let project_a = Uuid::now_v7();
        let projects = vec![entry(
            project_a,
            "Solo",
            ProjectStatus::Active,
            vec![],
            vec![],
            fixed_now(),
        )];
        let recent = vec![RecentEntity {
            id: Uuid::now_v7(),
            entity_type: EntityType::Note,
            title: "Intra-project note".into(),
            project_id: Some(project_a),
            tags: vec![],
            updated_at: fixed_now(),
            graph_links: vec![GraphLink {
                kind: "wiki_link".into(),
                neighbour_label: "Sibling note".into(),
                neighbour_project: Some(project_a),
            }],
        }];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &recent,
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 1.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(29);
        assert_eq!(NudgeSelector::new().select(&ctx, &mut rng), None);
    }

    #[test]
    fn fall_through_when_preferred_kind_has_no_candidates() {
        let project_id = Uuid::now_v7();
        let projects = vec![entry(
            project_id,
            "Has Questions",
            ProjectStatus::Active,
            vec!["only question".into()],
            vec![],
            fixed_now(),
        )];
        // Prefer cross-connection (weight 1.0) but no recent
        // entities — falls through to open_question.
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.3,
                    cross_connection: 1.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(13);
        let nudge = NudgeSelector::new().select(&ctx, &mut rng).unwrap();
        assert_eq!(nudge.kind, NudgeKind::OpenQuestion);
    }

    #[test]
    fn blocked_escalation_factor_multiplies_fire_probability() {
        let blocked_id = Uuid::now_v7();
        let projects = vec![entry(
            blocked_id,
            "Stuck",
            ProjectStatus::Blocked,
            vec![],
            vec![],
            fixed_now(),
        )];
        // base probability ~0.5 × 3 = 1.5 → clamped to 1.0.
        let config = IdeationConfig {
            tick_secs: 30,
            lambda_minutes: 1.0, // 60s mean → base p = 0.5
            blocked_escalation_factor: 3.0,
            ..base_config()
        };
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &config,
        };
        let mut rng = StdRng::seed_from_u64(17);
        for _ in 0..10 {
            assert!(
                should_fire_this_tick(&ctx, &mut rng),
                "escalated probability clamps to 1.0, so fire every tick"
            );
        }
    }

    #[test]
    fn zero_weights_produces_no_nudge() {
        let project_id = Uuid::now_v7();
        let projects = vec![entry(
            project_id,
            "X",
            ProjectStatus::Active,
            vec!["q".into()],
            vec![],
            fixed_now(),
        )];
        let ctx = NudgeContext {
            projects: &projects,
            recent_entities: &[],
            now: fixed_now(),
            config: &IdeationConfig {
                weights: crate::ideation::config::NudgeWeights {
                    open_question: 0.0,
                    cross_connection: 0.0,
                    blocked: 0.0,
                },
                ..base_config()
            },
        };
        let mut rng = StdRng::seed_from_u64(19);
        assert_eq!(NudgeSelector::new().select(&ctx, &mut rng), None);
    }
}
