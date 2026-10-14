//! Hash chain primitives.

/// A BLAKE3 digest.
pub type Hash = [u8; 32];

/// `prev_hash` of the first event.
pub const GENESIS: Hash = [0u8; 32];

/// Domain separator mixed into every chain hash.
pub const HASH_DOMAIN: &[u8] = b"wyrdline.ledger.v1/event";

/// `hash = BLAKE3(HASH_DOMAIN ‖ prev_hash ‖ canonical_cbor(event))`.
///
/// The canonical CBOR of an event contains its `seq`, timestamps, process
/// identity, kind, and payload, so all of them are covered.
pub fn chain_hash(prev: &Hash, event_cbor: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(HASH_DOMAIN);
    h.update(prev);
    h.update(event_cbor);
    *h.finalize().as_bytes()
}

pub(crate) fn hash_from_slice(bytes: &[u8]) -> Option<Hash> {
    bytes.try_into().ok()
}
