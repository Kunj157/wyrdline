//! The archive layer of a bundle: a deterministic ustar stream in one zstd
//! frame.
//!
//! Every entry is a regular file with a canonical header: mode `0644`,
//! uid/gid 0, empty user and group names, mtime 0, no link name, no prefix,
//! and the size in octal. The archive ends with two zero blocks. Equal
//! entries therefore always produce equal bytes, and standard tools
//! (`tar --zstd -tvf x.wyrd`) can list and extract a bundle.
//!
//! Compression uses `ruzstd`, a pure-Rust zstd implementation, so this
//! crate builds for `wasm32-unknown-unknown` (the Studio opens bundles in
//! the browser).

/// Size of a tar block.
pub const BLOCK: usize = 512;

/// Longest entry name: the ustar `name` field, without the `prefix` field.
pub const MAX_NAME: usize = 100;

/// Largest entry size a ustar header holds (11 octal digits).
pub const MAX_SIZE: u64 = 0o77_777_777_777;

const MODE: &[u8; 8] = b"0000644\0";
const ZERO_ID: &[u8; 8] = b"0000000\0";
const MTIME: &[u8; 12] = b"00000000000\0";
const MAGIC: &[u8; 8] = b"ustar\x0000";

/// The canonical header of a regular file `name` of `size` bytes, with
/// `typeflag` (`b'0'` for a regular file). `None` when the name is longer
/// than [`MAX_NAME`] or contains NUL, or the size does not fit.
///
/// Other type flags are only useful to build test archives.
pub fn header(name: &str, size: u64, typeflag: u8) -> Option<[u8; BLOCK]> {
    let name = name.as_bytes();
    if name.is_empty() || name.len() > MAX_NAME || name.contains(&0) || size > MAX_SIZE {
        return None;
    }
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name);
    h[100..108].copy_from_slice(MODE);
    h[108..116].copy_from_slice(ZERO_ID);
    h[116..124].copy_from_slice(ZERO_ID);
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(MTIME);
    h[156] = typeflag;
    h[257..265].copy_from_slice(MAGIC);
    h[329..337].copy_from_slice(ZERO_ID);
    h[337..345].copy_from_slice(ZERO_ID);
    let sum = checksum(&h);
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Some(h)
}

/// Header checksum: the byte sum with the checksum field read as spaces.
pub(crate) fn checksum(h: &[u8; BLOCK]) -> u32 {
    h.iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                u32::from(b' ')
            } else {
                u32::from(b)
            }
        })
        .sum()
}

/// Zero bytes after `len` bytes of content up to the next block boundary.
pub(crate) fn padding(len: u64) -> usize {
    let rem = usize::try_from(len % BLOCK as u64).unwrap_or(0);
    if rem == 0 { 0 } else { BLOCK - rem }
}

/// Write `entries` in the given order as a ustar archive. `None` when a
/// name or size does not fit a canonical header.
pub fn pack<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (name, data) in entries {
        out.extend_from_slice(&header(name, data.len() as u64, b'0')?);
        out.extend_from_slice(data);
        out.resize(out.len() + padding(data.len() as u64), 0);
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Some(out)
}

/// Compress `tar` into one zstd frame (deterministic for equal input).
pub fn compress(tar: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(tar, ruzstd::encoding::CompressionLevel::Fastest)
}

#[cfg(test)]
mod tests {
    // nosemgrep: wyrd-no-lint-allow-for-unwrap-on-input-paths -- inside #[cfg(test)] mod tests; the allow is test-only.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn header_fields_are_canonical() {
        let h = header("ledger.cbor-seq", 1234, b'0').unwrap();
        assert_eq!(&h[..15], b"ledger.cbor-seq");
        assert_eq!(&h[124..136], b"00000002322\0");
        assert_eq!(&h[257..263], b"ustar\0");
        let stored = std::str::from_utf8(&h[148..154]).unwrap();
        assert_eq!(u32::from_str_radix(stored, 8).unwrap(), checksum(&h));
    }

    #[test]
    fn rejects_names_and_sizes_that_do_not_fit() {
        assert!(header(&"a".repeat(MAX_NAME + 1), 0, b'0').is_none());
        assert!(header("", 0, b'0').is_none());
        assert!(header("a\0b", 0, b'0').is_none());
        assert!(header("a", MAX_SIZE + 1, b'0').is_none());
    }

    #[test]
    fn pack_pads_to_blocks_and_ends_with_two_zero_blocks() {
        let tar = pack([("a", &b"xyz"[..]), ("b", &[][..])]).unwrap();
        assert_eq!(tar.len(), 5 * BLOCK);
        assert!(tar[tar.len() - 2 * BLOCK..].iter().all(|&b| b == 0));
        assert_eq!(compress(&tar), compress(&tar));
    }

    #[test]
    fn standard_tar_readers_see_plain_files_with_fixed_metadata() {
        let tar = pack([("manifest.json", &b"{}"[..]), ("cas/ab", &[7u8; 700][..])]).unwrap();
        let mut archive = tar::Archive::new(&tar[..]);
        let mut seen = Vec::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let h = entry.header();
            assert_eq!(h.entry_type(), tar::EntryType::Regular);
            assert_eq!(h.mode().unwrap(), 0o644);
            assert_eq!(
                (h.uid().unwrap(), h.gid().unwrap(), h.mtime().unwrap()),
                (0, 0, 0)
            );
            seen.push((
                entry.path().unwrap().display().to_string(),
                h.size().unwrap(),
            ));
        }
        assert_eq!(seen, [("manifest.json".into(), 2), ("cas/ab".into(), 700)]);
    }
}
