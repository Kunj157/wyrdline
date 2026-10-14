//! Canonical CBOR encoding of events.
//!
//! Structs serialise their fields in declaration order and every map in the
//! schema is a `BTreeMap` (including `serde_json::Value` objects, which are
//! ordered because `preserve_order` is not enabled), so encoding the same
//! event always yields the same bytes. The ledger hashes these bytes.

use crate::Event;

/// Upper bound on the size of one encoded event accepted by the decoder.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("failed to encode event: {0}")]
    Encode(String),
    #[error("failed to decode event: {0}")]
    Decode(String),
    #[error("encoded event is {0} bytes, limit is {MAX_EVENT_BYTES}")]
    TooLarge(usize),
}

/// Encode an event to its canonical CBOR bytes.
pub fn to_canonical_cbor(event: &Event) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(256);
    ciborium::into_writer(event, &mut out).map_err(|e| CodecError::Encode(e.to_string()))?;
    if out.len() > MAX_EVENT_BYTES {
        return Err(CodecError::TooLarge(out.len()));
    }
    Ok(out)
}

/// Decode an event from CBOR. Never panics on malformed input.
pub fn from_canonical_cbor(bytes: &[u8]) -> Result<Event, CodecError> {
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(CodecError::TooLarge(bytes.len()));
    }
    ciborium::de::from_reader_with_recursion_limit(bytes, 64)
        .map_err(|e| CodecError::Decode(e.to_string()))
}
