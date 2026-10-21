//! The whole bundle reader on arbitrary bytes: zstd, ustar, signature,
//! manifest, file hashes, ledger chain. Must never panic and must stay
//! within the (reduced) limits.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wyrd_bundle::read::{Limits, Options, read};

fuzz_target!(|data: &[u8]| {
    let options = Options {
        limits: Limits {
            max_entries: 1_000,
            max_entry_bytes: 4 << 20,
            max_manifest_bytes: 1 << 20,
            max_total_bytes: 16 << 20,
            max_ratio: 1_000,
            ratio_after_bytes: 1 << 20,
            max_window_bytes: 8 << 20,
        },
        signer: None,
    };
    let _ = read(data, &options);
});
