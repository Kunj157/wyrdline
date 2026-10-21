//! The CBOR decoders of the ledger: `ledger.cbor-seq` records, the
//! canonical event encoding inside them, and the chain check over both
//! (which the bundle reader only reaches behind a valid signature).
#![no_main]

use libfuzzer_sys::fuzz_target;
use wyrd_bundle::read::{check_ledger, parse_record};
use wyrd_schema::{from_canonical_cbor, to_canonical_cbor};

fuzz_target!(|data: &[u8]| {
    // As a single event: decoding never panics; a decoded event re-encodes.
    if let Ok(event) = from_canonical_cbor(data) {
        let _ = to_canonical_cbor(&event);
    }
    // As a ledger: records, events, and the chain.
    let _ = check_ledger(data);
    // As a record sequence, whatever the events.
    let mut rest = data;
    while let Ok((event, _hash, tail)) = parse_record(rest) {
        let _ = from_canonical_cbor(event);
        rest = tail;
    }
});
