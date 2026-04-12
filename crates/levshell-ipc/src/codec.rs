//! Codec trait and the v1 [`JsonCodec`] implementation.
//!
//! The [`Codec`] trait keeps the wire format swappable: switching to
//! MessagePack later means writing one new struct that implements `Codec`
//! and pointing the daemon at it. The trait is intentionally NOT
//! object-safe — it uses generic methods so each call site is monomorphized
//! to its concrete message type with zero virtual dispatch overhead.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::{IpcError, Result};

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
