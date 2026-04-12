//! Production [`Module`] that ingests Sway IPC and drives the workspace
//! indicator. The pure rendering logic lives in [`super::indicator`]; this
//! file is just plumbing.
//!
//! Architecturally the module spawns one background task in `start()` that
//! owns the swayipc subscription stream and a second `Connection` for
//! follow-up queries. The runner's per-module event loop is unused (the
//! module subscribes to no bus events) — sway is the source of truth, and
//! events flow *out* through [`EventBus::publish`] and the
//! [`WidgetPublisher`].
//!
//! When sway is not reachable (e.g. running on a non-sway desktop or in CI)
//! `Connection::new()` fails and `start()` returns
//! [`ModuleError::Unavailable`]. The runner parks the module and never
//! spawns the background task.

use async_trait::async_trait;
use futures_util::StreamExt;
use levshell_core::{
    Event, EventBus, EventKind, Module, ModuleError, ModuleResult, WidgetDescriptor,
};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus};
use swayipc_async::{
    Connection, Event as SwayEvent, EventType, Workspace, WorkspaceChange, WindowChange,
};
use tokio::task::JoinHandle;

use super::indicator::{
    WorkspaceIndicatorState, WorkspaceInfo, WORKSPACE_WIDGET_ID, WORKSPACE_WIDGET_TYPE,
};

pub struct SwayWorkspaceModule {
    bus: EventBus,
    publisher: WidgetPublisher,
    task: Option<JoinHandle<()>>,
}

impl SwayWorkspaceModule {
    pub fn new(bus: EventBus, publisher: WidgetPublisher) -> Self {
        Self {
            bus,
            publisher,
            task: None,
        }
    }
}

fn convert(workspaces: Vec<Workspace>) -> Vec<WorkspaceInfo> {
    workspaces
        .into_iter()
        .map(|w| WorkspaceInfo {
            name: w.name,
            num: w.num,
            focused: w.focused,
            urgent: w.urgent,
            output: w.output,
        })
        .collect()
}

fn publish_indicator(
    publisher: &WidgetPublisher,
    workspaces: Vec<WorkspaceInfo>,
    focused_window: Option<String>,
) {
    let state = WorkspaceIndicatorState::from_workspaces(workspaces)
        .with_focused_window(focused_window);
    let msg = DaemonMessage::WidgetUpdate(state.into_widget_update(WidgetStatus::Normal));
    if let Err(e) = publisher.try_send(msg) {
        tracing::warn!(error = %e, "sway-workspace: failed to push WidgetUpdate (channel full or closed)");
    }
}

#[async_trait]
impl Module for SwayWorkspaceModule {
    fn name(&self) -> &str {
        "sway-workspace"
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: WORKSPACE_WIDGET_ID.into(),
            widget_type: WORKSPACE_WIDGET_TYPE.into(),
        }]
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        // This module is a *producer* of WorkspaceChanged / WindowFocused
        // events. It does not itself consume any bus events, so the runner
        // doesn't need to subscribe on its behalf.
        Vec::new()
    }

    async fn start(&mut self) -> ModuleResult<()> {
        // First connection: dedicated to the long-lived event subscription.
        // `subscribe` consumes the connection and hands back an EventStream.
        let sub_conn = Connection::new()
            .await
            .map_err(|e| ModuleError::Unavailable(format!("sway IPC unavailable: {e}")))?;

        // Second connection: kept alive inside the background task for
        // get_workspaces() queries triggered by each event.
        let mut query_conn = Connection::new()
            .await
            .map_err(|e| ModuleError::Failed(format!("sway query connection failed: {e}")))?;

        // Push the initial widget state so the bar shows something the
        // moment the shell connects, before any event has fired.
        let initial = query_conn
            .get_workspaces()
            .await
            .map_err(|e| ModuleError::Failed(format!("get_workspaces: {e}")))?;
        publish_indicator(&self.publisher, convert(initial), None);

        let events = sub_conn
            .subscribe([EventType::Workspace, EventType::Window])
            .await
            .map_err(|e| ModuleError::Failed(format!("sway subscribe: {e}")))?;

        let bus = self.bus.clone();
        let publisher = self.publisher.clone();

        let task = tokio::spawn(async move {
            run_event_loop(events, query_conn, bus, publisher).await;
        });
        self.task = Some(task);

        tracing::info!("sway-workspace module started");
        Ok(())
    }

    async fn stop(&mut self) -> ModuleResult<()> {
        if let Some(task) = self.task.take() {
            task.abort();
            // Awaiting an aborted task returns a JoinError(Cancelled);
            // we don't care, we just want the task gone.
            let _ = task.await;
        }
        Ok(())
    }
}

async fn run_event_loop(
    events: swayipc_async::EventStream,
    mut query_conn: Connection,
    bus: EventBus,
    publisher: WidgetPublisher,
) {
    tokio::pin!(events);

    let mut current_focused_window: Option<String> = None;

    while let Some(event) = events.next().await {
        let event = match event {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, "sway event stream error, exiting loop");
                break;
            }
        };

        match event {
            SwayEvent::Workspace(box_ev) => {
                let new_name = box_ev
                    .current
                    .as_ref()
                    .and_then(|n| n.name.clone())
                    .unwrap_or_default();

                if matches!(
                    box_ev.change,
                    WorkspaceChange::Focus
                        | WorkspaceChange::Init
                        | WorkspaceChange::Empty
                        | WorkspaceChange::Move
                        | WorkspaceChange::Rename
                        | WorkspaceChange::Reload
                        | WorkspaceChange::Urgent
                ) {
                    bus.publish(Event::WorkspaceChanged {
                        name: new_name,
                        focused_window: current_focused_window.clone(),
                    });

                    match query_conn.get_workspaces().await {
                        Ok(workspaces) => {
                            publish_indicator(
                                &publisher,
                                convert(workspaces),
                                current_focused_window.clone(),
                            );
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to refresh workspaces");
                        }
                    }
                }
            }
            SwayEvent::Window(box_ev) => {
                if matches!(box_ev.change, WindowChange::Focus) {
                    let title = box_ev.container.name.clone();
                    let app_id = box_ev.container.app_id.clone();
                    current_focused_window = title.clone();
                    bus.publish(Event::WindowFocused {
                        app_id,
                        title: title.clone().unwrap_or_default(),
                    });
                    if let Ok(workspaces) = query_conn.get_workspaces().await {
                        publish_indicator(
                            &publisher,
                            convert(workspaces),
                            current_focused_window.clone(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    tracing::info!("sway-workspace event loop exited");
}
