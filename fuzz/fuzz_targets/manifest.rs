//! The manifest decoder and its consistency checks. The first 32 bytes
//! are the signer key the manifest must name; the rest is the JSON.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wyrd_bundle::read::{Options, parse_manifest};

fuzz_target!(|data: &[u8]| {
    let Some((key, json)) = data.split_first_chunk::<32>() else {
        return;
    };
    let _ = parse_manifest(json, key, &Options::default());
});
