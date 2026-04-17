//! Ollama streaming chat client.
//!
//! One public entry point — [`chat_stream`] — POSTs a conversation to
//! `{endpoint}/api/chat` with `stream: true` and invokes the caller's
//! `on_token` callback for every assistant delta, then once more with
//! `done = true` when the stream closes.
//!
//! ## Wire format
//!
//! Ollama streams newline-delimited JSON (one object per line):
//!
//! ```text
//! {"model":"...","message":{"role":"assistant","content":"Hel"},"done":false}
//! {"model":"...","message":{"role":"assistant","content":"lo!"},"done":false}
//! {"model":"...","done":true, ...}
//! ```
//!
//! Chunks off `reqwest::Response::chunk()` are arbitrary byte
//! boundaries — a single chunk may contain half a line, a whole line,
//! or several lines plus a partial. The parser holds an incomplete-line
//! buffer and only dispatches a callback once a full `\n`-terminated
//! JSON object is in hand.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::config::RubberDuckConfig;

/// One chat message in the conversation array sent to Ollama.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    pub content: String,
}

/// Token delta emitted by [`chat_stream`] via the `on_token` callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDelta {
    /// Role string from Ollama — always `"assistant"` for replies.
    pub role: String,
    /// Substring appended to the assistant turn. Empty on the final
    /// `done = true` frame.
    pub delta: String,
    /// `true` on the last frame.
    pub done: bool,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("ollama returned HTTP {status}: {body}")]
    Status {
        status: u16,
        body: String,
    },
    #[error("malformed NDJSON line from ollama: {line}")]
    BadLine {
        line: String,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
}

#[derive(Deserialize)]
struct ChatFrame {
    #[serde(default)]
    message: Option<FrameMessage>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct FrameMessage {
    role: String,
    content: String,
}

/// Stream a chat completion from Ollama. `on_token` is invoked for
/// every parsed frame. The final frame has `done = true` and usually
/// an empty `delta`.
pub async fn chat_stream(
    cfg: &RubberDuckConfig,
    messages: &[ChatMessage],
    mut on_token: impl FnMut(TokenDelta),
) -> Result<(), ClientError> {
    let url = format!("{}/api/chat", cfg.endpoint.trim_end_matches('/'));
    let body = ChatRequest {
        model: &cfg.model,
        messages,
        stream: true,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
        .build()?;

    let mut response = client.post(&url).json(&body).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ClientError::Status {
            status: status.as_u16(),
            body,
        });
    }

    let mut buf = Vec::<u8>::new();
    while let Some(chunk) = response.chunk().await? {
        buf.extend_from_slice(&chunk);
        // Drain complete lines; keep any trailing partial in `buf`.
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            // Exclude the newline itself.
            let line_str = std::str::from_utf8(&line[..line.len() - 1])
                .unwrap_or("")
                .trim();
            if line_str.is_empty() {
                continue;
            }
            let frame: ChatFrame =
                serde_json::from_str(line_str).map_err(|e| ClientError::BadLine {
                    line: line_str.to_owned(),
                    source: e,
                })?;
            let (role, delta) = match frame.message {
                Some(m) => (m.role, m.content),
                None => ("assistant".to_owned(), String::new()),
            };
            on_token(TokenDelta {
                role,
                delta,
                done: frame.done,
            });
            if frame.done {
                return Ok(());
            }
        }
    }

    // Stream ended without a `done: true` frame. Synthesize one so the
    // caller can finalize the UI state.
    on_token(TokenDelta {
        role: "assistant".into(),
        delta: String::new(),
        done: true,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Tiny test HTTP server. Reads the request, writes a canned
    /// chunked-encoding response with the given NDJSON body split
    /// across `chunks`. Returns the socket addr the test should dial.
    async fn spawn_stub(body_chunks: Vec<&'static [u8]>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Discard the incoming request.
            let mut rx = [0u8; 4096];
            let _ = sock.read(&mut rx).await;
            // Write response headers.
            let headers =
                b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n";
            sock.write_all(headers).await.unwrap();
            for piece in body_chunks {
                let chunk_hdr = format!("{:x}\r\n", piece.len());
                sock.write_all(chunk_hdr.as_bytes()).await.unwrap();
                sock.write_all(piece).await.unwrap();
                sock.write_all(b"\r\n").await.unwrap();
            }
            sock.write_all(b"0\r\n\r\n").await.unwrap();
            sock.shutdown().await.ok();
        });
        addr
    }

    fn cfg_for(addr: SocketAddr) -> RubberDuckConfig {
        RubberDuckConfig {
            endpoint: format!("http://{addr}"),
            timeout_secs: 5,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn assembles_deltas_across_chunk_boundaries() {
        // Response split across four transport chunks that don't align
        // with NDJSON line boundaries — the client must stitch bytes
        // back together.
        let addr = spawn_stub(vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"d",
            b"one\":false}\n{\"message\":{\"role\":\"assistan",
            b"t\",\"content\":\"lo!\"},\"done\":false}\n{\"done\":tru",
            b"e}\n",
        ])
        .await;

        let mut tokens = Vec::new();
        chat_stream(
            &cfg_for(addr),
            &[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            |t| tokens.push(t),
        )
        .await
        .unwrap();

        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].delta, "Hel");
        assert_eq!(tokens[1].delta, "lo!");
        assert!(tokens[2].done);
        assert!(tokens[2].delta.is_empty());
    }

    #[tokio::test]
    async fn non_2xx_status_surfaces_status_error() {
        // Stub that returns HTTP 500.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut rx = [0u8; 4096];
            let _ = sock.read(&mut rx).await;
            sock.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 13\r\n\r\nmodel missing",
            )
            .await
            .unwrap();
            sock.shutdown().await.ok();
        });

        let err = chat_stream(
            &cfg_for(addr),
            &[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            |_| {},
        )
        .await
        .expect_err("expected status error");
        match err {
            ClientError::Status { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("model missing"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_line_surfaces_badline_error() {
        let addr = spawn_stub(vec![b"not json at all\n"]).await;
        let err = chat_stream(
            &cfg_for(addr),
            &[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            |_| {},
        )
        .await
        .expect_err("expected bad line");
        assert!(matches!(err, ClientError::BadLine { .. }));
    }

    #[tokio::test]
    async fn stream_without_done_synthesizes_final_frame() {
        let addr = spawn_stub(vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":false}\n",
        ])
        .await;
        let mut tokens = Vec::new();
        chat_stream(
            &cfg_for(addr),
            &[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            |t| tokens.push(t),
        )
        .await
        .unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].delta, "hi");
        assert!(tokens[1].done);
    }
}
