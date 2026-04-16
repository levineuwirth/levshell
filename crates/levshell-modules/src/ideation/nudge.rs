//! [`Nudge`] — the output of the ideation engine.
//!
//! A nudge is a single suggestion surfaced to the user. It carries the
//! project it relates to, the kind (open-question / cross-connection /
//! blocked-escalation), and the human-readable title + body. The
//! module publishes a nudge as [`levshell_core::Event::NudgeDelivered`]
//! for downstream consumers (notification renderer, logger, test hook).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Discriminant for the three v1 nudge flavors. Wire-serialized as the
/// matching snake_case string so `levshell-core`'s stringly-typed
/// `Event::NudgeDelivered` stays a leaf event.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NudgeKind {
    /// Drawn from a project's `open_questions` list.
    OpenQuestion,
    /// A tag/keyword overlap between a recently-synced entity and a
    /// project other than the one the entity already belongs to.
    CrossConnection,
    /// A blocked or long-stale project; body prompts for the smallest
    /// concrete next step.
    BlockedEscalation,
}

impl NudgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            NudgeKind::OpenQuestion => "open_question",
            NudgeKind::CrossConnection => "cross_connection",
            NudgeKind::BlockedEscalation => "blocked_escalation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nudge {
    pub project_id: Uuid,
    pub kind: NudgeKind,
    /// Short summary suitable for a notification title. Typically the
    /// project name.
    pub title: String,
    /// Body text. The open-question body is the question itself; the
    /// blocked body is an unblocking prompt; the cross-connection body
    /// names the overlapping entity.
    pub body: String,
}
