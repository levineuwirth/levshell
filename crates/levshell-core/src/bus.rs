//! Typed event bus.
//!
//! The bus is an in-process publish/subscribe primitive built on
//! [`tokio::sync::mpsc`]. Subscribers register a name, a set of [`EventKind`]s
//! they care about, and a channel capacity; the bus hands them back a
//! [`mpsc::Receiver<Event>`]. Publishers call [`EventBus::publish`] (a sync
//! call), which fans the event out to every matching subscriber via
//! `try_send`. A full subscriber channel is logged as a dropped event and a
//! warning — the publisher is never blocked. Subscribers whose receiver has
//! been dropped are silently removed on the next publish.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// All bus events Levshell publishes. New variants are added freely; the
/// `non_exhaustive` attribute prevents downstream crates from matching
/// exhaustively, leaving room for future additions.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The active Sway workspace changed.
    WorkspaceChanged {
        name: String,
        focused_window: Option<String>,
    },

    /// A window gained focus.
    WindowFocused {
        app_id: Option<String>,
        title: String,
    },

    /// An entity in the unified data store was inserted, updated, or deleted.
    ///
    /// `entity_type` is a string label rather than `levshell_data::EntityType`
    /// because `levshell-core` is intentionally a leaf crate with no internal
    /// dependencies. Producers should pass the same string the data crate
    /// uses (e.g. `"project"`, `"note"`, `"ref"`).
    DataStoreUpdated {
        entity_type: String,
        entity_id: Uuid,
    },

    /// The system transitioned between AC and battery power.
    PowerStateChanged { on_battery: bool },
}

/// A discriminant for filtering subscriptions without instantiating an [`Event`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventKind {
    WorkspaceChanged,
    WindowFocused,
    DataStoreUpdated,
    PowerStateChanged,
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Event::WorkspaceChanged { .. } => EventKind::WorkspaceChanged,
            Event::WindowFocused { .. } => EventKind::WindowFocused,
            Event::DataStoreUpdated { .. } => EventKind::DataStoreUpdated,
            Event::PowerStateChanged { .. } => EventKind::PowerStateChanged,
        }
    }
}

// ---------------------------------------------------------------------------
// EventBus
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Subscriber {
    name: String,
    kinds: HashSet<EventKind>,
    tx: mpsc::Sender<Event>,
    dropped: u64,
}

#[derive(Debug, Default)]
struct Inner {
    subscribers: Vec<Subscriber>,
}

/// In-process publish/subscribe bus. Cheap to clone — internally an
/// `Arc<Mutex<Inner>>`. Both publishers and the [`crate::ModuleRunner`]
/// hold clones of the same bus.
#[derive(Clone, Debug, Default)]
pub struct EventBus {
    inner: Arc<Mutex<Inner>>,
}

/// Per-subscriber statistics, returned from [`EventBus::stats`].
#[derive(Debug, Clone)]
pub struct SubscriberStats {
    pub name: String,
    pub kinds: HashSet<EventKind>,
    pub dropped: u64,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscriber. Returns the receiver end of a bounded
    /// channel of `capacity` slots. The subscriber will receive every
    /// published [`Event`] whose [`Event::kind`] is in `kinds`. When the
    /// returned receiver is dropped, the subscriber is removed from the bus
    /// on the next publish call.
    pub fn subscribe(
        &self,
        name: impl Into<String>,
        kinds: impl IntoIterator<Item = EventKind>,
        capacity: usize,
    ) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let mut inner = self.inner.lock().expect("event bus mutex poisoned");
        inner.subscribers.push(Subscriber {
            name: name.into(),
            kinds: kinds.into_iter().collect(),
            tx,
            dropped: 0,
        });
        rx
    }

    /// Publish an event to all matching subscribers. Non-blocking: a
    /// subscriber whose channel is full has the event dropped (logged as a
    /// warning) and the publisher proceeds. Subscribers whose receivers are
    /// closed are removed in this same pass.
    pub fn publish(&self, event: Event) {
        let kind = event.kind();
        let mut inner = self.inner.lock().expect("event bus mutex poisoned");
        inner.subscribers.retain_mut(|sub| {
            if !sub.kinds.contains(&kind) {
                return true;
            }
            match sub.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    sub.dropped = sub.dropped.saturating_add(1);
                    tracing::warn!(
                        subscriber = %sub.name,
                        kind = ?kind,
                        dropped = sub.dropped,
                        "event bus dropped event: subscriber lagging"
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(
                        subscriber = %sub.name,
                        "removing closed event subscriber"
                    );
                    false
                }
            }
        });
    }

    /// Snapshot of every active subscriber's stats. Mostly useful for
    /// diagnostics; not intended to be called in hot paths.
    pub fn stats(&self) -> Vec<SubscriberStats> {
        let inner = self.inner.lock().expect("event bus mutex poisoned");
        inner
            .subscribers
            .iter()
            .map(|s| SubscriberStats {
                name: s.name.clone(),
                kinds: s.kinds.clone(),
                dropped: s.dropped,
            })
            .collect()
    }

    /// Number of currently registered subscribers (after the most recent
    /// publish call's cleanup).
    pub fn subscriber_count(&self) -> usize {
        let inner = self.inner.lock().expect("event bus mutex poisoned");
        inner.subscribers.len()
    }
}
