#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Bundle reading: a round trip with export, and every way a hostile or
//! corrupted bundle is rejected, each with its specific error.

use std::collections::BTreeMap;
use std::io::Read as _;

use ed25519_dalek::SigningKey;
use wyrd_bundle::archive::{self, BLOCK};
use wyrd_bundle::format::{self, FileEntry, Manifest};
use wyrd_bundle::read::{
    CheckStatus, Limits, Options, ReadError, Stage, parse_record, read, verify,
};
use wyrd_bundle::{ExportInput, Redaction, SessionMeta, export};
use wyrd_schema::*;

fn ev(seq: u64, body: EventBody) -> Event {
    Event {
        seq,
        ts_mono_ns: seq * 1000,
        ts_wall_ms: 1_700_000_000_000 + i64::try_from(seq).unwrap(),
        process: None,
        drops_since_last: 0,
        body,
    }
}

fn events() -> Vec<Event> {
    let bodies = vec![
        EventBody::SessionStarted(SessionStarted {
            session_id: "01jread".into(),
            agent_kind: "fakeagent".into(),
            agent_version: None,
            command: vec!["agent".into()],
            workspace_root: "~/shop".into(),
            mode: CaptureMode::Cell,
            kernel_release: "6.8.0".into(),
            capabilities: BTreeMap::new(),
            policy_set_hash: None,
        }),
        EventBody::Message(Message {
            role: Role::Assistant,
            turn: 1,
            text: "Editing the cart.".into(),
        }),
        EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: "src/cart.ts".into(),
                in_cell: true,
                truncated: false,
            },
            bytes: 10,
        }),
        EventBody::CheckpointCreated(CheckpointCreated {
            checkpoint_id: "c1".into(),
            branch_id: "a".into(),
            parent_id: None,
            after_call_id: None,
            manifest: format!("b3:{}", "ab".repeat(32)),
            files_changed: 1,
            bytes_added: 10,
            duration_ms: 1,
        }),
        EventBody::Message(Message {
            role: Role::Assistant,
            turn: 2,
            text: "All tests pass.".into(),
        }),
    ];
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, b)| ev(i as u64 + 1, b))
        .collect()
}

fn meta() -> SessionMeta {
    SessionMeta {
        id: "01jread".into(),
        name: Some("cart".into()),
        agent: "fakeagent".into(),
        mode: CaptureMode::Cell,
        status: SessionState::Closed,
        created_ms: 1_700_000_000_000,
    }
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn chunks() -> BTreeMap<[u8; 32], Vec<u8>> {
    [b"first file".to_vec(), vec![0u8; 3000]]
        .into_iter()
        .map(|b| (*blake3::hash(&b).as_bytes(), b))
        .collect()
}

fn good() -> Vec<u8> {
    let m = meta();
    let ev = events();
    let ch = chunks();
    export(
        &ExportInput {
            session: &m,
            events: &ev,
            chunks: &ch,
            content: true,
            withheld_files: 0,
            redaction: Redaction::Standard,
            divergences: wyrd_divergence::view::project_json,
            annotations: None,
        },
        &key(7),
    )
    .unwrap()
    .bytes
}

fn opts() -> Options {
    Options::default()
}

fn err(bytes: &[u8]) -> ReadError {
    read(bytes, &opts()).expect_err("must be rejected")
}

/// The archive entries of a bundle, in order.
fn untar(bundle: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut tar = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(bundle)
        .unwrap()
        .read_to_end(&mut tar)
        .unwrap();
    let mut a = tar::Archive::new(&tar[..]);
    a.entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let name = e.path().unwrap().display().to_string();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            (name, data)
        })
        .collect()
}

/// Compress raw `(name, typeflag, data)` entries, no checks.
fn raw(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, flag, data) in entries {
        tar.extend_from_slice(&archive::header(name, data.len() as u64, *flag).unwrap());
        tar.extend_from_slice(data);
        tar.resize(tar.len().next_multiple_of(BLOCK), 0);
    }
    tar.resize(tar.len() + 2 * BLOCK, 0);
    archive::compress(&tar)
}

/// Re-sign a bundle made of `files` (manifest file list recomputed) after
/// `edit` changes the manifest.
fn resign(
    files: &BTreeMap<String, Vec<u8>>,
    signer: &SigningKey,
    edit: impl FnOnce(&mut Manifest),
) -> Vec<u8> {
    let entries = untar(&good());
    let mut m: Manifest = serde_json::from_slice(&entries[0].1).unwrap();
    let pk = signer.verifying_key().to_bytes();
    m.signer.public_key = hex::encode(pk);
    m.signer.fingerprint = format::fingerprint(&pk);
    m.files = files
        .iter()
        .map(|(p, b)| FileEntry {
            path: p.clone(),
            size: b.len() as u64,
            blake3: format::b3(b),
        })
        .collect();
    edit(&mut m);
    let mut json = serde_json::to_vec_pretty(&m).unwrap();
    json.push(b'\n');
    let mut sig = pk.to_vec();
    sig.extend_from_slice(
        &ed25519_dalek::Signer::sign(signer, &format::signed_message(&json)).to_bytes(),
    );
    let all: Vec<(&str, &[u8])> = [(format::MANIFEST, &json[..]), (format::SIGNATURE, &sig[..])]
        .into_iter()
        .chain(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
        .collect();
    archive::compress(&archive::pack(all).unwrap())
}

fn content_files() -> BTreeMap<String, Vec<u8>> {
    untar(&good()).into_iter().skip(2).collect()
}

/// `ledger.cbor-seq` with event `seq`'s message text replaced by one of the
/// same length, its stored chain hash kept.
fn tampered_ledger(ledger: &[u8], seq: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = ledger;
    let mut n = 0;
    while !rest.is_empty() {
        n += 1;
        let (cbor, hash, tail) = parse_record(rest).unwrap();
        rest = tail;
        let mut e = from_canonical_cbor(cbor).unwrap();
        if n == seq {
            let EventBody::Message(m) = &mut e.body else {
                panic!("event {seq} is not a message")
            };
            m.text = "All tests fail.".into();
        }
        let cbor = to_canonical_cbor(&e).unwrap();
        ciborium::into_writer(
            &ciborium::Value::Array(vec![
                ciborium::Value::Bytes(cbor),
                ciborium::Value::Bytes(hash.to_vec()),
            ]),
            &mut out,
        )
        .unwrap();
    }
    out
}

#[test]
fn an_exported_bundle_reads_back_whole() {
    let b = read(&good()[..], &opts()).unwrap();
    assert_eq!(b.events, events());
    assert_eq!(b.chunks, chunks());
    assert_eq!(b.public_key, key(7).verifying_key().to_bytes());
    assert_eq!(b.manifest.session, meta());
    assert_eq!(b.manifest.ledger.events, 5);
    assert_eq!(b.fingerprint(), b.manifest.signer.fingerprint);
    let entries = untar(&good());
    assert_eq!(b.manifest_hash, format::b3(&entries[0].1));
    assert_eq!(
        b.claims_json,
        entries.iter().find(|e| e.0 == format::CLAIMS).unwrap().1
    );
    assert!(b.annotations.is_none());

    let v = verify(&good()[..], &opts());
    assert!(v.ok());
    assert!(v.checks.iter().all(|c| c.status == CheckStatus::Passed));
    assert_eq!(v.checks.len(), Stage::ALL.len());
}

#[test]
fn a_pinned_signer_is_enforced() {
    let pinned = Options {
        signer: Some(key(7).verifying_key().to_bytes()),
        ..opts()
    };
    assert!(read(&good()[..], &pinned).is_ok());
    let other = Options {
        signer: Some(key(8).verifying_key().to_bytes()),
        ..opts()
    };
    assert!(matches!(
        read(&good()[..], &other),
        Err(ReadError::UnexpectedSigner { .. })
    ));
}

#[test]
fn a_bad_signature_fails_before_anything_else_is_read() {
    let entries = untar(&good());
    let mut manifest = entries[0].1.clone();
    let i = manifest.iter().position(|&b| b == b'5').unwrap();
    manifest[i] = b'6';
    // Whatever follows is not even looked at: here a symlink.
    let b = raw(&[
        (format::MANIFEST, b'0', &manifest),
        (format::SIGNATURE, b'0', &entries[1].1),
        ("ledger.cbor-seq", b'2', b""),
    ]);
    let v = verify(&b[..], &opts());
    assert_eq!(v.result.as_ref().unwrap_err(), &ReadError::BadSignature);
    let status: Vec<_> = v.checks.iter().map(|c| (c.stage, c.status)).collect();
    assert_eq!(
        status,
        [
            (Stage::Archive, CheckStatus::Skipped),
            (Stage::Signature, CheckStatus::Failed),
            (Stage::Manifest, CheckStatus::Skipped),
            (Stage::Files, CheckStatus::Skipped),
            (Stage::Chain, CheckStatus::Skipped),
        ]
    );
    assert!(
        v.checks[1]
            .detail
            .as_ref()
            .unwrap()
            .contains("does not verify")
    );

    let short = raw(&[
        (format::MANIFEST, b'0', &entries[0].1),
        (format::SIGNATURE, b'0', b"xy"),
    ]);
    assert_eq!(err(&short), ReadError::SignatureSize(2));
    let swapped = raw(&[
        (format::SIGNATURE, b'0', &entries[1].1),
        (format::MANIFEST, b'0', &entries[0].1),
    ]);
    assert!(matches!(
        err(&swapped),
        ReadError::MissingEntry {
            expected: "manifest.json",
            ..
        }
    ));
}

#[test]
fn a_tampered_event_is_named_by_its_seq() {
    let mut files = content_files();
    let ledger = files[format::LEDGER].clone();
    let bad = tampered_ledger(&ledger, 5);
    assert_eq!(bad.len(), ledger.len());

    // Edited in place: the manifest hash no longer matches, and the reader
    // names the event that changed.
    let entries = untar(&good());
    let in_place: Vec<(&str, u8, &[u8])> = entries
        .iter()
        .map(|(n, d)| {
            let d = if n == format::LEDGER { &bad } else { d };
            (n.as_str(), b'0', d.as_slice())
        })
        .collect();
    let e = err(&raw(&in_place));
    assert_eq!(e, ReadError::EventModified { seq: 5 });
    assert_eq!(e.stage(), Stage::Files);

    // Re-signed by someone else with a fixed-up manifest: the chain
    // recomputation names seq 5.
    files.insert(format::LEDGER.into(), bad);
    let e = err(&resign(&files, &key(9), |_| {}));
    assert_eq!(e, ReadError::HashMismatch { seq: 5 });
    assert_eq!(e.stage(), Stage::Chain);
    assert_eq!(e.seq(), Some(5));
}

#[test]
fn a_tampered_chunk_is_named() {
    let entries = untar(&good());
    let chunk = entries.iter().find(|e| e.0.starts_with("cas/")).unwrap();
    let name = chunk.0.trim_start_matches("cas/").to_string();
    let mut changed = chunk.1.clone();
    changed[0] ^= 1;
    let in_place: Vec<(&str, u8, &[u8])> = entries
        .iter()
        .map(|(n, d)| {
            let d = if *n == chunk.0 { &changed } else { d };
            (n.as_str(), b'0', d.as_slice())
        })
        .collect();
    assert_eq!(
        err(&raw(&in_place)),
        ReadError::ChunkModified {
            chunk: name.clone()
        }
    );

    // Listed with its real hash but under another name.
    let mut files = content_files();
    let bytes = files.remove(&chunk.0).unwrap();
    let wrong = format!("cas/{}", "0".repeat(64));
    files.insert(wrong, bytes);
    let e = err(&resign(&files, &key(7), |_| {}));
    assert_eq!(
        e,
        ReadError::ChunkName {
            chunk: "0".repeat(64)
        }
    );
}

#[test]
fn chain_head_and_length_must_match_the_manifest() {
    let files = content_files();
    let e = err(&resign(&files, &key(7), |m| m.ledger.events = 4));
    assert!(matches!(
        e,
        ReadError::HeadMismatch {
            expected: 4,
            actual: 5,
            ..
        }
    ));
    let e = err(&resign(&files, &key(7), |m| {
        m.ledger.head_hash = format!("b3:{}", "cd".repeat(32));
    }));
    assert!(matches!(e, ReadError::HeadMismatch { .. }));

    // A record that is not [bytes, bytes(32)].
    let mut files = content_files();
    files.insert(format::LEDGER.into(), vec![0x82, 0x41, 0x00, 0x41, 0x00]);
    let e = err(&resign(&files, &key(7), |m| m.ledger.events = 1));
    assert!(matches!(e, ReadError::BadRecord { seq: 1, .. }), "{e:?}");
}

#[test]
fn unknown_versions_and_fields_are_refused() {
    let files = content_files();
    let e = err(&resign(&files, &key(7), |m| m.format_version = 2));
    assert_eq!(
        e,
        ReadError::UnknownVersion {
            format: "wyrdline.bundle".into(),
            version: 2
        }
    );
    let e = err(&resign(&files, &key(7), |m| m.format = "other".into()));
    assert!(matches!(e, ReadError::UnknownVersion { .. }));

    // An unknown field (signed): build the JSON by hand.
    let entries = untar(&good());
    let mut value: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
    value["extra"] = serde_json::json!(1);
    let json = serde_json::to_vec(&value).unwrap();
    let signer = key(7);
    let mut sig = signer.verifying_key().to_bytes().to_vec();
    sig.extend_from_slice(
        &ed25519_dalek::Signer::sign(&signer, &format::signed_message(&json)).to_bytes(),
    );
    let bundle = raw(&[
        (format::MANIFEST, b'0', &json),
        (format::SIGNATURE, b'0', &sig),
    ]);
    assert!(matches!(err(&bundle), ReadError::BadManifest(msg) if msg.contains("extra")));
}

#[test]
fn the_manifest_must_be_consistent() {
    let files = content_files();
    let e = err(&resign(&files, &key(7), |m| {
        m.signer.fingerprint = "0000000000000000".into();
    }));
    assert!(matches!(e, ReadError::SignerMismatch { .. }));
    let e = err(&resign(&files, &key(7), |m| m.files.reverse()));
    assert!(matches!(e, ReadError::BadManifest(_)));
    let e = err(&resign(&files, &key(7), |m| {
        let dup = m.files[0].clone();
        m.files.insert(0, dup);
    }));
    assert!(matches!(e, ReadError::DuplicateEntry { .. }));
    let e = err(&resign(&files, &key(7), |m| {
        m.files[0].path = "../x".into();
    }));
    assert!(matches!(e, ReadError::PathTraversal { .. }));
    let e = err(&resign(&files, &key(7), |m| m.content.included = false));
    assert!(matches!(e, ReadError::BadManifest(_)));
    let e = err(&resign(&files, &key(7), |m| {
        m.files.retain(|f| f.path != format::CLAIMS);
    }));
    assert!(matches!(e, ReadError::BadManifest(m) if m.contains("claims.json")));
}

/// A valid signed prefix (manifest + signature) followed by `rest`.
fn after_signed_prefix(rest: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let entries = untar(&good());
    let mut all: Vec<(&str, u8, &[u8])> = vec![
        (format::MANIFEST, b'0', &entries[0].1),
        (format::SIGNATURE, b'0', &entries[1].1),
    ];
    all.extend_from_slice(rest);
    raw(&all)
}

#[test]
fn paths_links_devices_and_duplicates_are_rejected() {
    let cases: Vec<(&str, u8, ReadError)> = vec![
        (
            "../../etc/cron.d/x",
            b'0',
            ReadError::PathTraversal {
                path: "../../etc/cron.d/x".into(),
            },
        ),
        (
            "cas/../../x",
            b'0',
            ReadError::PathTraversal {
                path: "cas/../../x".into(),
            },
        ),
        (
            "/etc/passwd",
            b'0',
            ReadError::AbsolutePath {
                path: "/etc/passwd".into(),
            },
        ),
        (
            "notes.txt",
            b'0',
            ReadError::InvalidPath {
                path: "notes.txt".into(),
            },
        ),
        (
            "claims.json",
            b'2',
            ReadError::Symlink {
                path: "claims.json".into(),
            },
        ),
        (
            "claims.json",
            b'1',
            ReadError::Hardlink {
                path: "claims.json".into(),
            },
        ),
        (
            "claims.json",
            b'3',
            ReadError::Device {
                path: "claims.json".into(),
            },
        ),
        (
            "claims.json",
            b'4',
            ReadError::Device {
                path: "claims.json".into(),
            },
        ),
        (
            "claims.json",
            b'6',
            ReadError::Device {
                path: "claims.json".into(),
            },
        ),
        (
            "cas",
            b'5',
            ReadError::UnsupportedEntry {
                path: "cas".into(),
                typeflag: '5',
            },
        ),
        (
            "claims.json",
            b'x',
            ReadError::UnsupportedEntry {
                path: "claims.json".into(),
                typeflag: 'x',
            },
        ),
        (
            "manifest.json",
            b'0',
            ReadError::DuplicateEntry {
                path: "manifest.json".into(),
            },
        ),
    ];
    for (name, flag, expected) in cases {
        let e = err(&after_signed_prefix(&[(name, flag, b"")]));
        assert_eq!(e, expected, "{name} {}", char::from(flag));
        assert_eq!(e.stage(), Stage::Archive);
    }
}

#[test]
fn bad_entries_are_rejected_anywhere_and_duplicates_too() {
    // Also as the very first entry, before any signature.
    let e = err(&raw(&[("../x", b'0', b"")]));
    assert_eq!(
        e,
        ReadError::PathTraversal {
            path: "../x".into()
        }
    );
    let e = err(&raw(&[(format::MANIFEST, b'2', b"")]));
    assert_eq!(
        e,
        ReadError::Symlink {
            path: "manifest.json".into()
        }
    );

    // The same file twice among the content.
    let entries = untar(&good());
    let mut all: Vec<(&str, u8, &[u8])> = entries
        .iter()
        .map(|(n, d)| (n.as_str(), b'0', d.as_slice()))
        .collect();
    all.insert(3, all[2]);
    assert!(matches!(err(&raw(&all)), ReadError::DuplicateEntry { .. }));
}

#[test]
fn entries_must_follow_the_manifest() {
    let entries = untar(&good());
    let all: Vec<(&str, u8, &[u8])> = entries
        .iter()
        .map(|(n, d)| (n.as_str(), b'0', d.as_slice()))
        .collect();

    let mut missing = all.clone();
    missing.pop();
    assert_eq!(
        err(&raw(&missing)),
        ReadError::MissingFile {
            path: format::LEDGER.into()
        }
    );

    let mut swapped = all.clone();
    swapped.swap(2, 3);
    assert!(matches!(err(&raw(&swapped)), ReadError::OutOfOrder { .. }));

    let mut extra = all.clone();
    extra.push((format::ANNOTATIONS, b'0', b"x"));
    assert_eq!(
        err(&raw(&extra)),
        ReadError::UnexpectedEntry {
            path: format::ANNOTATIONS.into()
        }
    );

    let mut short = all.clone();
    let shorter = &all[2].2[1..];
    short[2].2 = shorter;
    assert!(matches!(err(&raw(&short)), ReadError::SizeMismatch { .. }));
}

#[test]
fn size_bombs_and_limits() {
    // An entry header claiming more than the limit is refused unread.
    let entries = untar(&good());
    let small = Options {
        limits: Limits {
            max_manifest_bytes: 100,
            ..Limits::default()
        },
        ..opts()
    };
    assert!(matches!(
        read(&good()[..], &small),
        Err(ReadError::EntryTooLarge { path, .. }) if path == "manifest.json"
    ));

    // Megabytes of zeros after the archive: total and ratio limits.
    let mut tar = Vec::new();
    for (n, d) in &entries {
        tar.extend_from_slice(&archive::header(n, d.len() as u64, b'0').unwrap());
        tar.extend_from_slice(d);
        tar.resize(tar.len().next_multiple_of(BLOCK), 0);
    }
    tar.resize(tar.len() + 8 * 1024 * 1024, 0);
    let bomb = archive::compress(&tar);
    assert!(bomb.len() < 64 * 1024, "{}", bomb.len());
    let total = Options {
        limits: Limits {
            max_total_bytes: 1 << 20,
            ..Limits::default()
        },
        ..opts()
    };
    assert_eq!(
        read(&bomb[..], &total).unwrap_err(),
        ReadError::TotalTooLarge { limit: 1 << 20 }
    );
    let ratio = Options {
        limits: Limits {
            max_ratio: 100,
            ratio_after_bytes: 1 << 20,
            ..Limits::default()
        },
        ..opts()
    };
    assert_eq!(
        read(&bomb[..], &ratio).unwrap_err(),
        ReadError::CompressionRatio { limit: 100 }
    );
    // Within the defaults, trailing zero padding is accepted.
    assert!(read(&bomb[..], &opts()).is_ok());

    let few = Options {
        limits: Limits {
            max_entries: 3,
            ..Limits::default()
        },
        ..opts()
    };
    assert!(matches!(
        read(&good()[..], &few),
        Err(ReadError::TooManyEntries { limit: 3 })
    ));
}

#[test]
fn not_a_bundle_truncated_or_trailing_data() {
    assert!(matches!(
        err(b"PK\x03\x04 not zstd"),
        ReadError::Decompress(_)
    ));
    assert!(matches!(err(b""), ReadError::Decompress(_)));
    let g = good();
    let cut = &g[..g.len() / 2];
    assert!(matches!(
        err(cut),
        ReadError::Decompress(_) | ReadError::Truncated
    ));
    let mut trailing = g.clone();
    trailing.extend_from_slice(b"junk");
    assert_eq!(err(&trailing), ReadError::TrailingData);
    // A tar of random-looking headers.
    let mut tar = vec![b'x'; BLOCK];
    tar.extend_from_slice(&[0u8; 2 * BLOCK]);
    assert!(matches!(
        err(&archive::compress(&tar)),
        ReadError::BadHeader { index: 0, .. }
    ));
    // An empty archive has no manifest.
    let empty = archive::compress(&[0u8; 2 * BLOCK]);
    assert!(matches!(
        err(&empty),
        ReadError::MissingEntry {
            expected: "manifest.json",
            ..
        }
    ));
}

#[test]
fn annotations_travel_in_standard_bundles_only() {
    let m = meta();
    let ev = events();
    let ch = BTreeMap::new();
    let doc = b"\x85\x6f\x4a\x83automerge document bytes".to_vec();
    let with = |redaction| {
        export(
            &ExportInput {
                session: &m,
                events: &ev,
                chunks: &ch,
                content: false,
                withheld_files: 0,
                redaction,
                divergences: wyrd_divergence::view::project_json,
                annotations: Some(&doc),
            },
            &key(7),
        )
        .unwrap()
        .bytes
    };
    let standard = read(&with(Redaction::Standard)[..], &opts()).unwrap();
    assert_eq!(standard.annotations.as_deref(), Some(doc.as_slice()));
    assert!(
        standard
            .manifest
            .files
            .iter()
            .any(|f| f.path == format::ANNOTATIONS)
    );
    // Free text from teammates cannot be redacted: strict leaves it out.
    let strict = read(&with(Redaction::Strict)[..], &opts()).unwrap();
    assert!(strict.annotations.is_none());
}
