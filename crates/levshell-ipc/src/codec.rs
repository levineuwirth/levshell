//! Codec trait and the v1 [`JsonCodec`] implementation.
//!
//! The [`Codec`] trait keeps the wire format swappable: switching to
//! MessagePack later means writing one new struct that implements `Codec`
//! and pointing the daemon at it. The trait is intentionally NOT
//! object-safe — it uses generic methods so each call site is monomorphized
//! to its concrete message type with zero virtual dispatch overhead.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::{IpcError, Result};

/// # Invariant: encoded output must not contain `0x0a` (newline).
///
/// The transport in `framing.rs` is newline-delimited, so any codec plugged
/// in here must guarantee its `encode` output is free of raw `\n` bytes —
/// otherwise a single message would be split across multiple frames on the
/// shell side. `JsonCodec` satisfies this trivially (compact serde_json
/// escapes newlines inside strings as `\n` and never emits them between
/// tokens). A future MessagePack or FlatBuffers codec would need base64,
/// hex, or a different framing layer to preserve the invariant.
///
/// `write_frame` enforces the check at runtime as a defensive backstop, but
/// a correct codec should never hit it.
pub trait Codec: Send + Sync + 'static {
    fn encode<M>(&self, msg: &M) -> Result<Vec<u8>>
    where
        M: Serialize + ?Sized;

    fn decode<M>(&self, bytes: &[u8]) -> Result<M>
    where
        M: DeserializeOwned;
}

/// JSON codec built on `serde_json`. The default codec for v1 — QML/JavaScript
/// parses JSON natively, so the QML side never needs an extra dependency.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn encode<M>(&self, msg: &M) -> Result<Vec<u8>>
    where
        M: Serialize + ?Sized,
    {
        serde_json::to_vec(msg).map_err(IpcError::codec)
    }

    fn decode<M>(&self, bytes: &[u8]) -> Result<M>
    where
        M: DeserializeOwned,
    {
        serde_json::from_slice(bytes).map_err(IpcError::codec)
    }
}
