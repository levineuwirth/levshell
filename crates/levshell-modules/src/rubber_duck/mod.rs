//! Rubber-duck debugger module (spec §2.12.6).
//!
//! A minimal chat surface backed by a local LLM (Ollama by default).
//! The user types a stuck-point; the "duck" asks short, specific
//! clarifying questions to help them articulate it. The chat
//! interface itself is a QML overlay — this module owns conversation
//! state and the streaming HTTP bridge to Ollama.
//!
//! ## Wire shape
//!
//! - User typing → `ShellMessage::DuckSay { text }` → bus
//!   `Event::DuckUserMessage` → this module.
//! - Ctl / keybind → `CtlRequest::Duck { action }` → bus
//!   `Event::DuckActionRequested` → this module (open / close / reset).
//! - Outbound: `DaemonMessage::DuckOpen`, `DuckClose`, `DuckReset` for
//!   lifecycle; `DaemonMessage::DuckToken { role, delta, done }` for
//!   streaming replies.
//!
//! ## State
//!
//! Conversation is in-memory, resets on daemon restart. Reset via ctl
//! wipes the Vec<Message> and restarts from the system prompt. No
//! persistence — the duck is meant for transient stuck points, not
//! long-running sessions.

pub mod client;
pub mod config;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, DuckStatus, DuckToken, WidgetPublisher};

use client::{ChatMessage, TokenDelta};

pub use client::ClientError;
pub use config::{default_rubber_duck_config_path, RubberDuckConfig, RubberDuckConfigError};

pub const MODULE_NAME: &str = "rubber-duck";

/// How we cap a single runaway conversation. Ollama responds with one
/// delta per token, so the conversation array grows by one entry per
/// user-turn + one per assistant-turn. A hard cap keeps a locked
/// rubber-duck session from accumulating unbounded memory.
const MAX_MESSAGES: usize = 128;

pub struct RubberDuckModule {
    publisher: WidgetPublisher,
    config: RubberDuckConfig,
    /// Conversation messages, mirrored on the daemon side. Protected
    /// by a `std::sync::Mutex` rather than `tokio::sync::Mutex` because
    /// every critical section is short (push a message, clone the vec
    /// for the streaming task) and no section crosses an `.await`.
    conversation: Arc<Mutex<Vec<ChatMessage>>>,
}

impl RubberDuckModule {
    pub fn new(publisher: WidgetPublisher) -> Self {
        Self::with_config(publisher, RubberDuckConfig::default())
    }

    pub fn with_config(publisher: WidgetPublisher, config: RubberDuckConfig) -> Self {
        Self {
            publisher,
            config,
            conversation: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn publish(&self, msg: DaemonMessage) {
        if let Err(e) = self.publisher.try_send(msg) {
            tracing::warn!(error = %e, "rubber-duck: publish drop");
        }
    }

    /// Build a [`DuckStatus`] from the active config. `reachable` /
    /// `detail` describe the most recent backend interaction.
    fn status_msg(cfg: &RubberDuckConfig, reachable: bool, detail: String) -> DaemonMessage {
        DaemonMessage::DuckStatus(DuckStatus {
            enabled: cfg.enabled,
            reachable,
            endpoint: cfg.endpoint.clone(),
            model: cfg.model.clone(),
            detail,
        })
    }

    fn reset_conversation(&self) {
        let mut guard = self.conversation.lock().expect("rubber-duck convo lock");
        guard.clear();
    }

    /// Append a user turn and return a clone of the full conversation
    /// (with the system prompt prepended) to send to Ollama. Returns
    /// `None` when the conversation has exceeded [`MAX_MESSAGES`] —
    /// the caller should publish a reset and refuse.
    fn push_user(&self, text: String) -> Option<Vec<ChatMessage>> {
        let mut guard = self.conversation.lock().expect("rubber-duck convo lock");
        if guard.len() >= MAX_MESSAGES {
            return None;
        }
        guard.push(ChatMessage {
            role: "user".into(),
            content: text,
        });
        // System prompt is synthetic — lives in config, not the convo
        // vec. Build the full message list here.
        let mut full = Vec::with_capacity(guard.len() + 1);
        full.push(ChatMessage {
            role: "system".into(),
            content: self.config.system_prompt.clone(),
        });
        full.extend(guard.iter().cloned());
        Some(full)
    }

    async fn handle_user_message(&mut self, text: String) {
        if !self.config.enabled {
            tracing::debug!("rubber-duck: disabled in config; ignoring message");
            self.publish(Self::status_msg(
                &self.config,
                false,
                "disabled in config".into(),
            ));
            return;
        }
        if text.trim().is_empty() {
            return;
        }

        let messages = match self.push_user(text) {
            Some(m) => m,
            None => {
                tracing::warn!(
                    limit = MAX_MESSAGES,
                    "rubber-duck: conversation at cap; auto-resetting"
                );
                self.reset_conversation();
                self.publish(DaemonMessage::DuckReset);
                return;
            }
        };

        let cfg = self.config.clone();
        let publisher = self.publisher.clone();
        let convo = self.conversation.clone();
        // Spawn so the module's event loop isn't blocked during the
        // (potentially multi-second) streaming reply.
        tokio::spawn(async move {
            let mut accumulated = String::new();
            let result = client::chat_stream(&cfg, &messages, |delta: TokenDelta| {
                if !delta.delta.is_empty() {
                    accumulated.push_str(&delta.delta);
                }
                let msg = DaemonMessage::DuckToken(DuckToken {
                    role: delta.role,
                    delta: delta.delta,
                    done: delta.done,
                });
                if let Err(e) = publisher.try_send(msg) {
                    tracing::warn!(error = %e, "rubber-duck: token drop");
                }
            })
            .await;

            match result {
                Ok(()) => {
                    let mut guard = convo.lock().expect("rubber-duck convo lock");
                    if !accumulated.is_empty() {
                        guard.push(ChatMessage {
                            role: "assistant".into(),
                            content: accumulated,
                        });
                    }
                    drop(guard);
                    // Clear any prior unreachable banner on recovery.
                    let _ = publisher
                        .try_send(Self::status_msg(&cfg, true, String::new()));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "rubber-duck: ollama request failed");
                    // Emit a synthetic token so the UI doesn't hang
                    // waiting for a `done` frame.
                    let msg = DaemonMessage::DuckToken(DuckToken {
                        role: "assistant".into(),
                        delta: format!("(rubber-duck error: {e})"),
                        done: true,
                    });
                    let _ = publisher.try_send(msg);
                    // Drive the dedicated banner with the real reason.
                    let _ = publisher.try_send(Self::status_msg(
                        &cfg,
                        false,
                        e.to_string(),
                    ));
                }
            }
        });
    }

    fn handle_action(&mut self, action: &str) {
        match action {
            "open" => {
                self.publish(DaemonMessage::DuckOpen);
                // Optimistic — `reachable: true` until a send fails;
                // surfaces the banner immediately when disabled.
                self.publish(Self::status_msg(&self.config, true, String::new()));
            }
            "close" => self.publish(DaemonMessage::DuckClose),
            "reset" => {
                self.reset_conversation();
                self.publish(DaemonMessage::DuckReset);
            }
            other => {
                tracing::debug!(action = other, "rubber-duck: ignoring unknown action");
            }
        }
    }
}

#[async_trait]
impl Module for RubberDuckModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        Vec::new()
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::DuckActionRequested, EventKind::DuckUserMessage]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        Ok(())
    }

    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        match event {
            Event::DuckActionRequested { action } => self.handle_action(action),
            Event::DuckUserMessage { text } => {
                self.handle_user_message(text.clone()).await;
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
    use levshell_ipc::{spawn_writer_task, IpcWriter, JsonCodec};
    use tokio::io::{duplex, AsyncReadExt, BufReader};

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

    async fn read_frame_json(reader: &mut BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
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

    #[tokio::test]
    async fn action_open_publishes_duck_open() {
        let (publisher, _handle, mut reader) = writer_over_duplex();
        let mut m = RubberDuckModule::new(publisher);
        m.on_event(&Event::DuckActionRequested {
            action: "open".into(),
        })
        .await
        .unwrap();
        let v = read_frame_json(&mut reader).await;
        assert_eq!(v["type"], "duck_open");
    }

    #[tokio::test]
    async fn action_reset_clears_conversation_and_notifies_shell() {
        let (publisher, _handle, mut reader) = writer_over_duplex();
        let mut m = RubberDuckModule::new(publisher);
        // Pre-populate conversation.
        m.conversation.lock().unwrap().push(ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        });
        m.on_event(&Event::DuckActionRequested {
            action: "reset".into(),
        })
        .await
        .unwrap();
        assert!(m.conversation.lock().unwrap().is_empty());
        let v = read_frame_json(&mut reader).await;
        assert_eq!(v["type"], "duck_reset");
    }

    #[tokio::test]
    async fn disabled_config_ignores_user_message() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let cfg = RubberDuckConfig {
            enabled: false,
            ..Default::default()
        };
        let mut m = RubberDuckModule::with_config(publisher, cfg);
        m.on_event(&Event::DuckUserMessage {
            text: "stuck".into(),
        })
        .await
        .unwrap();
        assert!(m.conversation.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_message_is_ignored() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let mut m = RubberDuckModule::new(publisher);
        m.on_event(&Event::DuckUserMessage {
            text: "   ".into(),
        })
        .await
        .unwrap();
        assert!(m.conversation.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn push_user_appends_and_includes_system_prompt() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let cfg = RubberDuckConfig {
            system_prompt: "SYS".into(),
            ..Default::default()
        };
        let m = RubberDuckModule::with_config(publisher, cfg);
        let full = m.push_user("why is X broken".into()).unwrap();
        assert_eq!(full.len(), 2);
        assert_eq!(full[0].role, "system");
        assert_eq!(full[0].content, "SYS");
        assert_eq!(full[1].role, "user");
        assert_eq!(full[1].content, "why is X broken");
        // Conversation vec records only the user turn, not the system
        // prompt (which is synthesized per request from config).
        assert_eq!(m.conversation.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn push_user_returns_none_at_cap() {
        let (publisher, _handle, _reader) = writer_over_duplex();
        let m = RubberDuckModule::new(publisher);
        {
            let mut guard = m.conversation.lock().unwrap();
            for _ in 0..MAX_MESSAGES {
                guard.push(ChatMessage {
                    role: "user".into(),
                    content: "x".into(),
                });
            }
        }
        assert!(m.push_user("more".into()).is_none());
    }

}
