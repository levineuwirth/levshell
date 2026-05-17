//! Freedesktop notification bridge for spec design §9 rule 3.
//!
//! Telemetry modules emit [`Event::CriticalEscalation`] when a widget's
//! escalation tracker crosses into `Critical`. This module subscribes
//! to that event and fans it out to:
//!
//! 1. the **shell**, as `DaemonMessage::CriticalEscalation` — powers
//!    the in-bar full-sat flash even when the user looks up mid-tick
//!    (though the `WidgetUpdate`'s `escalation` field already does the
//!    bulk of the visual work);
//! 2. the **Freedesktop notification daemon**, via `notify-rust` — the
//!    spec's "escape hatch when the widget is hidden." Sent with
//!    Urgency::Critical so compositors that honour it keep the toast
//!    up until dismissed.
//!
//! The OS-side call is abstracted behind [`NotificationSender`] so
//! tests can inject a spy and assert on call patterns without touching
//! D-Bus. Production wires in [`NotifyRustSender`].
//!
//! Send failures are logged and swallowed — a headless environment
//! without a running notification daemon still gets the shell channel
//! + tracing log, which is enough for the user to notice on resume.

use std::sync::Arc;

use async_trait::async_trait;
use levshell_core::{Event, EventKind, Module, ModuleResult};
use levshell_ipc::{CriticalEscalation, DaemonMessage, Nudge, WidgetPublisher};

const MODULE_NAME: &str = "notifications";

/// Abstraction over the OS-side notification call so tests don't need
/// a live D-Bus session. Implementations are expected to be cheap to
/// call (production impl does all work in a blocking D-Bus round-trip,
/// which is acceptable at Critical cadence — a Critical entry happens
/// at most every few seconds per widget).
pub trait NotificationSender: Send + Sync {
    fn send_critical(&self, widget_id: &str, title: &str, body: &str)
        -> Result<(), String>;

    /// Surface an ideation nudge. Normal urgency (not Critical) — this
    /// is a gentle prompt, not an alert. Goes through the same
    /// Freedesktop path, which `levshell`'s own NotificationServer owns,
    /// so the nudge lands in the notification center / popup.
    fn send_nudge(&self, kind: &str, title: &str) -> Result<(), String>;

    /// Surface a generic, ctl-originated notification (spec §2.19.1,
    /// `levshell-ctl notify ...`). `urgency` is `"low"`, `"normal"`, or
    /// `"critical"`. The default impl is a no-op so test doubles and
    /// future senders need not implement it explicitly.
    fn send_notify(&self, _urgency: &str, _title: &str, _body: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Production sender: invokes `notify-rust`.
pub struct NotifyRustSender;

impl NotificationSender for NotifyRustSender {
    fn send_critical(
        &self,
        _widget_id: &str,
        title: &str,
        body: &str,
    ) -> Result<(), String> {
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .urgency(notify_rust::Urgency::Critical)
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send_nudge(&self, kind: &str, title: &str) -> Result<(), String> {
        notify_rust::Notification::new()
            .summary(title)
            .body(&format!("Ideation · {kind}"))
            .urgency(notify_rust::Urgency::Normal)
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send_notify(&self, urgency: &str, title: &str, body: &str) -> Result<(), String> {
        let u = match urgency {
            "low" => notify_rust::Urgency::Low,
            "critical" => notify_rust::Urgency::Critical,
            _ => notify_rust::Urgency::Normal,
        };
        notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .urgency(u)
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

pub struct NotificationsModule {
    publisher: WidgetPublisher,
    sender: Arc<dyn NotificationSender>,
}

impl NotificationsModule {
    pub fn new(publisher: WidgetPublisher, sender: Arc<dyn NotificationSender>) -> Self {
        Self { publisher, sender }
    }

    /// Production constructor — wires in [`NotifyRustSender`].
    pub fn with_notify_rust(publisher: WidgetPublisher) -> Self {
        Self::new(publisher, Arc::new(NotifyRustSender))
    }
}

#[async_trait]
impl Module for NotificationsModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![
            EventKind::CriticalEscalation,
            EventKind::NudgeDelivered,
            EventKind::NotifyRequested,
        ]
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::CriticalEscalation {
                widget_id,
                title,
                body,
            } => {
                let msg = DaemonMessage::CriticalEscalation(CriticalEscalation {
                    widget_id: widget_id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                });
                if let Err(e) = self.publisher.try_send(msg) {
                    tracing::warn!(
                        error = %e,
                        widget_id = %widget_id,
                        "notifications: shell channel drop"
                    );
                }
                let sender = Arc::clone(&self.sender);
                let (w, t, b) = (widget_id.clone(), title.clone(), body.clone());
                let res = tokio::task::spawn_blocking(move || {
                    sender.send_critical(&w, &t, &b)
                })
                .await;
                match res {
                    Ok(Err(e)) => tracing::warn!(
                        error = %e,
                        widget_id = %widget_id,
                        "notifications: OS notification send failed"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        widget_id = %widget_id,
                        "notifications: OS notification task panicked"
                    ),
                    Ok(Ok(())) => {}
                }
            }
            // Ideation nudges go out two ways: the Freedesktop path
            // (for the notification center / other daemons) AND a
            // dedicated DaemonMessage::Nudge so the shell can show a
            // transient toast even when another daemon owns
            // org.freedesktop.Notifications (spec §2.9.2).
            Event::NudgeDelivered { kind, title, .. } => {
                if let Err(e) = self.publisher.try_send(DaemonMessage::Nudge(Nudge {
                    kind: kind.clone(),
                    title: title.clone(),
                })) {
                    tracing::warn!(
                        error = %e,
                        kind = %kind,
                        "notifications: nudge shell channel drop"
                    );
                }
                let sender = Arc::clone(&self.sender);
                let (k, t) = (kind.clone(), title.clone());
                let res = tokio::task::spawn_blocking(move || {
                    sender.send_nudge(&k, &t)
                })
                .await;
                match res {
                    Ok(Err(e)) => tracing::warn!(
                        error = %e,
                        kind = %kind,
                        "notifications: nudge send failed"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        kind = %kind,
                        "notifications: nudge task panicked"
                    ),
                    Ok(Ok(())) => {}
                }
            }
            // Generic ctl-originated notification (spec §2.19.1). No
            // DaemonMessage — the Freedesktop path is the surface, same
            // as nudges.
            Event::NotifyRequested {
                title,
                body,
                urgency,
            } => {
                let sender = Arc::clone(&self.sender);
                let (u, t, b) = (urgency.clone(), title.clone(), body.clone());
                let res = tokio::task::spawn_blocking(move || {
                    sender.send_notify(&u, &t, &b)
                })
                .await;
                match res {
                    Ok(Err(e)) => tracing::warn!(
                        error = %e,
                        "notifications: ctl notify send failed"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "notifications: ctl notify task panicked"
                    ),
                    Ok(Ok(())) => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use std::sync::Mutex;
    use tokio::io::duplex;

    struct SpySender {
        calls: Mutex<Vec<(String, String, String)>>,
        nudges: Mutex<Vec<(String, String)>>,
        notifies: Mutex<Vec<(String, String, String)>>,
        result: Mutex<Result<(), String>>,
    }

    impl SpySender {
        fn with_result(result: Result<(), String>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                nudges: Mutex::new(Vec::new()),
                notifies: Mutex::new(Vec::new()),
                result: Mutex::new(result),
            }
        }

        fn calls(&self) -> Vec<(String, String, String)> {
            self.calls.lock().unwrap().clone()
        }

        fn nudges(&self) -> Vec<(String, String)> {
            self.nudges.lock().unwrap().clone()
        }

        fn notifies(&self) -> Vec<(String, String, String)> {
            self.notifies.lock().unwrap().clone()
        }
    }

    impl NotificationSender for SpySender {
        fn send_critical(
            &self,
            widget_id: &str,
            title: &str,
            body: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push((
                widget_id.to_string(),
                title.to_string(),
                body.to_string(),
            ));
            self.result.lock().unwrap().clone()
        }

        fn send_nudge(&self, kind: &str, title: &str) -> Result<(), String> {
            self.nudges
                .lock()
                .unwrap()
                .push((kind.to_string(), title.to_string()));
            self.result.lock().unwrap().clone()
        }

        fn send_notify(
            &self,
            urgency: &str,
            title: &str,
            body: &str,
        ) -> Result<(), String> {
            self.notifies.lock().unwrap().push((
                urgency.to_string(),
                title.to_string(),
                body.to_string(),
            ));
            self.result.lock().unwrap().clone()
        }
    }

    async fn module_with_spy(
        spy: Arc<SpySender>,
    ) -> (NotificationsModule, tokio::sync::mpsc::Receiver<DaemonMessage>) {
        // A live publisher whose writer task we drop immediately; we
        // snoop the DaemonMessage::CriticalEscalation forwarding via
        // the writer task's inbound channel by spawning a loopback.
        let (a, b) = duplex(4096);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 16);
        // Spawn a reader that decodes frames off `b`.
        let (fwd_tx, fwd_rx) = tokio::sync::mpsc::channel::<DaemonMessage>(16);
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut b = b;
            let mut buf = Vec::new();
            let mut scratch = [0u8; 1024];
            loop {
                let n = match b.read(&mut scratch).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                buf.extend_from_slice(&scratch[..n]);
                while let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    let line: Vec<u8> = buf.drain(..=nl).collect();
                    if let Ok(msg) =
                        serde_json::from_slice::<DaemonMessage>(&line[..line.len() - 1])
                    {
                        let _ = fwd_tx.send(msg).await;
                    }
                }
            }
        });
        let module = NotificationsModule::new(task.publisher, spy);
        (module, fwd_rx)
    }

    #[tokio::test]
    async fn on_event_forwards_notify_to_sender() {
        let spy = Arc::new(SpySender::with_result(Ok(())));
        let (mut module, _fwd) = module_with_spy(spy.clone()).await;
        module
            .on_event(&Event::NotifyRequested {
                title: "Build finished".into(),
                body: "cargo build: ok".into(),
                urgency: "normal".into(),
            })
            .await
            .unwrap();

        let n = spy.notifies();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "normal");
        assert_eq!(n[0].1, "Build finished");
        assert_eq!(n[0].2, "cargo build: ok");
        // A generic notify is not a critical escalation — no DaemonMessage.
        assert!(spy.calls().is_empty());
    }

    #[tokio::test]
    async fn on_event_forwards_critical_to_sender_and_publisher() {
        let spy = Arc::new(SpySender::with_result(Ok(())));
        let (mut module, mut fwd) = module_with_spy(spy.clone()).await;
        module
            .on_event(&Event::CriticalEscalation {
                widget_id: "cpu".into(),
                title: "CPU critically high".into(),
                body: "CPU sustained at 96%".into(),
            })
            .await
            .unwrap();

        let msg = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fwd.recv(),
        )
        .await
        .expect("expected DaemonMessage within 200ms")
        .expect("channel open");
        match msg {
            DaemonMessage::CriticalEscalation(p) => {
                assert_eq!(p.widget_id, "cpu");
                assert_eq!(p.title, "CPU critically high");
                assert_eq!(p.body, "CPU sustained at 96%");
            }
            other => panic!("unexpected DaemonMessage: {other:?}"),
        }

        let calls = spy.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "cpu");
        assert_eq!(calls[0].1, "CPU critically high");
    }

    #[tokio::test]
    async fn on_event_ignores_unrelated_events() {
        let spy = Arc::new(SpySender::with_result(Ok(())));
        let (mut module, _fwd) = module_with_spy(spy.clone()).await;
        module
            .on_event(&Event::PowerStateChanged { on_battery: true })
            .await
            .unwrap();
        assert!(spy.calls().is_empty());
    }

    #[tokio::test]
    async fn sender_failure_does_not_fail_on_event() {
        let spy = Arc::new(SpySender::with_result(Err("no dbus".into())));
        let (mut module, _fwd) = module_with_spy(spy.clone()).await;
        let result = module
            .on_event(&Event::CriticalEscalation {
                widget_id: "battery".into(),
                title: "Battery critically low".into(),
                body: "Battery at 3%".into(),
            })
            .await;
        assert!(result.is_ok(), "send failure must be swallowed");
        assert_eq!(spy.calls().len(), 1);
    }

    #[tokio::test]
    async fn module_subscribes_to_critical_escalation() {
        let spy: Arc<dyn NotificationSender> =
            Arc::new(SpySender::with_result(Ok(())));
        let (a, _b) = duplex(32);
        let w = IpcWriter::from_parts(a, JsonCodec);
        let task = spawn_writer_task(w, 4);
        let m = NotificationsModule::new(task.publisher, spy);
        let subs = m.subscribed_events();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&EventKind::CriticalEscalation));
        assert!(subs.contains(&EventKind::NudgeDelivered));
        assert!(subs.contains(&EventKind::NotifyRequested));
    }

    #[tokio::test]
    async fn on_event_forwards_nudge_to_sender_and_shell() {
        let spy = Arc::new(SpySender::with_result(Ok(())));
        let (mut module, mut fwd) = module_with_spy(spy.clone()).await;
        module
            .on_event(&Event::NudgeDelivered {
                project_id: uuid::Uuid::nil(),
                kind: "open_question".into(),
                title: "Revisit the attention-sparsity assumption".into(),
            })
            .await
            .unwrap();
        let nudges = spy.nudges();
        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].0, "open_question");
        assert_eq!(nudges[0].1, "Revisit the attention-sparsity assumption");
        // Nudges must NOT go through the critical sender/publisher.
        assert!(spy.calls().is_empty());

        // …but they DO get a dedicated DaemonMessage::Nudge so the shell
        // can toast even when another daemon owns Freedesktop (§2.9.2).
        let msg = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fwd.recv(),
        )
        .await
        .expect("expected DaemonMessage within 200ms")
        .expect("channel open");
        match msg {
            DaemonMessage::Nudge(n) => {
                assert_eq!(n.kind, "open_question");
                assert_eq!(n.title, "Revisit the attention-sparsity assumption");
            }
            other => panic!("unexpected DaemonMessage: {other:?}"),
        }
    }
}
