//! Hash-chain primitives shared by the ledger and the bundle reader.
//!
//! They live here, next to the canonical encoding they hash, so that code
//! without SQLite (the bundle reader, the Studio through WebAssembly) can
//! recompute a chain exactly as the ledger does.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_hash_is_domain_separated_and_order_sensitive() {
        let a = chain_hash(&GENESIS, b"x");
        assert_ne!(a, *blake3::hash(b"x").as_bytes());
        assert_ne!(chain_hash(&a, b"y"), chain_hash(&GENESIS, b"y"));
    }
}
