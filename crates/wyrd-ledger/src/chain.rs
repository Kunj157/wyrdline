//! Hash chain primitives (defined in `wyrd-schema`, see [`wyrd_schema::chain`]).

pub use wyrd_schema::chain::{GENESIS, HASH_DOMAIN, Hash, chain_hash};

pub(crate) fn hash_from_slice(bytes: &[u8]) -> Option<Hash> {
    bytes.try_into().ok()
}
