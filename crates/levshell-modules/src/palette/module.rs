//! [`PaletteModule`] — the runtime driver that wires the providers to
//! the bus and the shell.
//!
//! ## Lifecycle
//!
//! 1. `start()` publishes an initial closed-state `WidgetUpdate` so the
//!    shell has something to bind against before any event fires.
//! 2. The module subscribes to:
//!    * [`Event::PaletteActionRequested`] — open/close/toggle from ctl
//!    * [`Event::CommandPaletteQueryReceived`] — live query from the shell
//!    * [`Event::CommandPaletteSelectReceived`] — user picked an item
//! 3. On each event, the module updates its internal state (open?,
//!    current query, current result set) and re-publishes the widget
//!    payload. No ticking; the palette is strictly event-driven.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::join_all;
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, EscalationLevel, WidgetPublisher, WidgetStatus, WidgetUpdate};

use super::provider::{merge_results, PaletteItem, PaletteProvider, PaletteState};

pub const PALETTE_WIDGET_ID: &str = "command-palette";
pub const PALETTE_WIDGET_TYPE: &str = "command_palette";

/// Global cap on merged results across all providers. Generous so the
/// user can scroll through the full app launcher with an empty query;
/// the per-provider ceilings (256 apps, 10 FTS note hits, ~all
/// workspaces) already bound the input. QML's ListView handles
/// hundreds of rows comfortably, so this is a safety ceiling for
/// pathological systems rather than a UX cap.
const MAX_RESULTS: usize = 256;

pub struct PaletteModule {
    publisher: WidgetPublisher,
    providers: Vec<Box<dyn PaletteProvider>>,
    state: PaletteState,
}

impl PaletteModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self {
            publisher,
            providers: Vec::new(),
            state: PaletteState::default(),
        }
    }

    pub fn with_provider(mut self, provider: Box<dyn PaletteProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    pub fn with_providers(mut self, providers: Vec<Box<dyn PaletteProvider>>) -> Self {
        self.providers.extend(providers);
        self
    }

    fn publish(&self) {
        let value = match serde_json::to_value(&self.state) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "palette: failed to serialize state");
                return;
            }
        };
        let update = WidgetUpdate {
            widget_id: PALETTE_WIDGET_ID.into(),
            widget_type: PALETTE_WIDGET_TYPE.into(),
            state: value,
            status: WidgetStatus::Normal,
            escalation: EscalationLevel::Ambient,
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "palette: failed to publish WidgetUpdate");
        }
    }

    async fn refresh_results(&mut self) {
        if self.providers.is_empty() {
            self.state.results = Vec::new();
            return;
        }
        let query = self.state.query.clone();
        let futures = self
            .providers
            .iter()
            .map(|p| p.search(&query))
            .collect::<Vec<_>>();
        let buckets: Vec<Vec<PaletteItem>> = join_all(futures).await;
        self.state.results = merge_results(buckets, MAX_RESULTS);
    }

    async fn apply_palette_action(&mut self, action: &str, query_seed: Option<String>) {
        match action {
            "open" => {
                self.state.open = true;
                if let Some(q) = query_seed {
                    self.state.query = q;
                }
                self.refresh_results().await;
            }
            "close" => {
                self.state.open = false;
                self.state.query.clear();
                self.state.results.clear();
            }
            "toggle" => {
                if self.state.open {
                    self.state.open = false;
                    self.state.query.clear();
                    self.state.results.clear();
                } else {
                    self.state.open = true;
                    self.refresh_results().await;
                }
            }
            "query" => {
                if let Some(q) = query_seed {
                    self.state.query = q;
                    if self.state.open {
                        self.refresh_results().await;
                    }
                }
            }
            other => {
                tracing::debug!(action = other, "palette: ignoring unknown action");
            }
        }
    }

    async fn apply_query(&mut self, query: String) {
        self.state.query = query;
        if self.state.open {
            self.refresh_results().await;
        }
    }

    async fn apply_select(&mut self, provider: &str, item_id: &str) {
        // Find the target provider by name. We tolerate the case where
        // the provider is gone — log and close the palette.
        let target = self.providers.iter().find(|p| p.name() == provider);
        let Some(target) = target else {
            tracing::warn!(provider, "palette: select for unknown provider");
            return;
        };
        if let Err(e) = target.execute(item_id).await {
            tracing::warn!(error = %e, provider, item_id, "palette: execute failed");
        }
        // Close the palette after a successful (or attempted) execute.
        self.state.open = false;
        self.state.query.clear();
        self.state.results.clear();
    }
}

#[async_trait]
impl Module for PaletteModule {
    fn name(&self) -> &str {
        "command-palette"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: PALETTE_WIDGET_ID.into(),
            widget_type: PALETTE_WIDGET_TYPE.into(),
        }]
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::PaletteActionRequested,
            EventKind::CommandPaletteQueryReceived,
            EventKind::CommandPaletteSelectReceived,
        ]
    }

    fn tick_interval(&self) -> Option<Duration> {
        None
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // Publish an initial closed-state widget so the shell has
        // something to bind against before any event fires.
        self.publish();
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::PaletteActionRequested { action, query } => {
                self.apply_palette_action(action, query.clone()).await;
                self.publish();
            }
            Event::CommandPaletteQueryReceived { query } => {
                self.apply_query(query.clone()).await;
                self.publish();
            }
            Event::CommandPaletteSelectReceived { provider, item_id } => {
                self.apply_select(provider, item_id).await;
                self.publish();
            }
            _ => {}
        }
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::provider::ProviderResult;
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::{duplex, AsyncReadExt, BufReader};

    /// Minimal stub provider that returns a fixed list of items.
    struct StubProvider {
        name: &'static str,
        items: Vec<PaletteItem>,
    }

    #[async_trait]
    impl PaletteProvider for StubProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn search(&self, _query: &str) -> Vec<PaletteItem> {
            self.items.clone()
        }
        async fn execute(&self, _item_id: &str) -> ProviderResult<()> {
            Ok(())
        }
    }

    fn writer_over_duplex() -> (
        WidgetPublisher,
        tokio::task::JoinHandle<()>,
        BufReader<tokio::io::DuplexStream>,
    ) {
        let (a, b) = duplex(4096);
        let writer = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(writer, 16);
        (task.publisher, task.handle, BufReader::new(b))
    }

    async fn read_frame_json(
        reader: &mut BufReader<tokio::io::DuplexStream>,
    ) -> serde_json::Value {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            reader.read_exact(&mut byte).await.unwrap();
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        serde_json::from_slice(&buf).unwrap()
    }

    fn make_provider(
        name: &'static str,
        items: &[(&str, &str)],
    ) -> Box<dyn PaletteProvider> {
        let items = items
            .iter()
            .map(|(id, title)| PaletteItem::new(name, *id, *title).with_score(0.8))
            .collect();
        Box::new(StubProvider { name, items })
    }

    #[tokio::test]
    async fn start_publishes_closed_palette_state() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher);
        module.start().await.unwrap();

        let frame = read_frame_json(&mut reader).await;
        assert_eq!(
            frame.get("type").and_then(|v| v.as_str()),
            Some("widget_update")
        );
        assert_eq!(
            frame.get("widget_id").and_then(|v| v.as_str()),
            Some("command-palette")
        );
        let state = frame.get("state").unwrap();
        assert_eq!(state.get("open").and_then(|v| v.as_bool()), Some(false));
        let results = state.get("results").and_then(|v| v.as_array()).unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn palette_action_open_populates_results_from_providers() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher).with_providers(vec![make_provider(
            "apps",
            &[("firefox", "Firefox"), ("gedit", "Text Editor")],
        )]);
        module.start().await.unwrap();
        // Drain the initial closed-state frame.
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "open".into(),
                query: None,
            })
            .await
            .unwrap();

        let frame = read_frame_json(&mut reader).await;
        let state = frame.get("state").unwrap();
        assert_eq!(state.get("open").and_then(|v| v.as_bool()), Some(true));
        let results = state.get("results").and_then(|v| v.as_array()).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn query_event_refreshes_results_when_open() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher).with_providers(vec![make_provider(
            "apps",
            &[("firefox", "Firefox"), ("gedit", "Text Editor")],
        )]);
        module.start().await.unwrap();
        // Drain initial closed-state frame.
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "open".into(),
                query: None,
            })
            .await
            .unwrap();
        // Drain open frame.
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::CommandPaletteQueryReceived {
                query: "fire".into(),
            })
            .await
            .unwrap();
        let frame = read_frame_json(&mut reader).await;
        let state = frame.get("state").unwrap();
        assert_eq!(state.get("query").and_then(|v| v.as_str()), Some("fire"));
        // The stub provider returns both entries regardless of query —
        // we're just verifying that a query event triggers a refresh +
        // publish.
        let results = state.get("results").and_then(|v| v.as_array()).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn close_event_clears_state() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher).with_providers(vec![make_provider(
            "apps",
            &[("firefox", "Firefox")],
        )]);
        module.start().await.unwrap();
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "open".into(),
                query: None,
            })
            .await
            .unwrap();
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "close".into(),
                query: None,
            })
            .await
            .unwrap();
        let frame = read_frame_json(&mut reader).await;
        let state = frame.get("state").unwrap();
        assert_eq!(state.get("open").and_then(|v| v.as_bool()), Some(false));
        let results = state.get("results").and_then(|v| v.as_array()).unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn toggle_event_opens_then_closes() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher).with_providers(vec![make_provider(
            "apps",
            &[("a", "Alpha")],
        )]);
        module.start().await.unwrap();
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "toggle".into(),
                query: None,
            })
            .await
            .unwrap();
        let frame = read_frame_json(&mut reader).await;
        assert_eq!(
            frame
                .get("state")
                .and_then(|s| s.get("open"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        module
            .on_event(&Event::PaletteActionRequested {
                action: "toggle".into(),
                query: None,
            })
            .await
            .unwrap();
        let frame = read_frame_json(&mut reader).await;
        assert_eq!(
            frame
                .get("state")
                .and_then(|s| s.get("open"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[tokio::test]
    async fn select_event_closes_palette_after_dispatch() {
        let (publisher, _h, mut reader) = writer_over_duplex();
        let mut module = PaletteModule::new(publisher).with_providers(vec![make_provider(
            "apps",
            &[("firefox", "Firefox")],
        )]);
        module.start().await.unwrap();
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::PaletteActionRequested {
                action: "open".into(),
                query: None,
            })
            .await
            .unwrap();
        let _ = read_frame_json(&mut reader).await;

        module
            .on_event(&Event::CommandPaletteSelectReceived {
                provider: "apps".into(),
                item_id: "firefox".into(),
            })
            .await
            .unwrap();
        let frame = read_frame_json(&mut reader).await;
        let state = frame.get("state").unwrap();
        assert_eq!(state.get("open").and_then(|v| v.as_bool()), Some(false));
    }
}
