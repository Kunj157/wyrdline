#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The Studio's bundle mode, natively: verification report, events,
//! checkpoint diffs (equal to the Cell's), and file contents from `cas/`.

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use serde_bytes::ByteBuf;
use serde_json::{Value, json};
use wyrd_bundle::{ExportInput, Redaction, SessionMeta, export};
use wyrd_cas::{ChunkHash, Entry, EntryKind, Manifest};
use wyrd_schema::*;
use wyrd_wasm::bundle::open;

fn file(path: &str, content: &[u8]) -> Entry {
    Entry {
        path: ByteBuf::from(path.as_bytes().to_vec()),
        kind: EntryKind::File,
        mode: 0o644,
        size: content.len() as u64,
        mtime_ns: 0,
        chunks: vec![ChunkHash::of(content)],
        target: None,
        opaque: false,
    }
}

fn entry(path: &str, kind: EntryKind) -> Entry {
    Entry {
        path: ByteBuf::from(path.as_bytes().to_vec()),
        kind,
        mode: 0o755,
        size: 0,
        mtime_ns: 0,
        chunks: vec![],
        target: (kind == EntryKind::Symlink).then(|| ByteBuf::from(b"cart.ts".to_vec())),
        opaque: false,
    }
}

const SECRET: &[u8] = b"withheld secret content";

/// Two checkpoints: c2 modifies `src/cart.ts`, adds `src/new.ts` and a
/// symlink, deletes `README.md` (whiteout), and touches a file whose
/// chunk is not in the bundle (withheld).
fn manifests() -> (Manifest, Manifest) {
    let c1 = Manifest::new(vec![
        entry("src", EntryKind::Dir),
        file("src/cart.ts", b"let total = 0;\n"),
        file("src/keep.ts", b"same\n"),
    ])
    .unwrap();
    let c2 = Manifest::new(vec![
        entry("README.md", EntryKind::Whiteout),
        entry("src", EntryKind::Dir),
        file("src/cart.ts", b"let total = 1;\n"),
        entry("src/link", EntryKind::Symlink),
        file("src/keep.ts", b"same\n"),
        file("src/new.ts", b"export {};\n"),
        file("src/.env", SECRET),
    ])
    .unwrap();
    (c1, c2)
}

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

fn checkpoint(id: &str, parent: Option<&str>, m: &Manifest) -> EventBody {
    EventBody::CheckpointCreated(CheckpointCreated {
        checkpoint_id: id.into(),
        branch_id: "a".into(),
        parent_id: parent.map(Into::into),
        after_call_id: None,
        manifest: ChunkHash::of(&m.to_cbor().unwrap()).to_string(),
        files_changed: 1,
        bytes_added: 1,
        duration_ms: 1,
    })
}

fn bundle(content: bool) -> (Vec<u8>, Manifest, Manifest) {
    let (c1, c2) = manifests();
    let events: Vec<Event> = vec![
        EventBody::SessionStarted(SessionStarted {
            session_id: "01jwasm".into(),
            agent_kind: "fakeagent".into(),
            agent_version: None,
            command: vec!["agent".into()],
            workspace_root: "~/shop".into(),
            mode: CaptureMode::Cell,
            kernel_release: "6.8.0".into(),
            capabilities: BTreeMap::new(),
            policy_set_hash: None,
        }),
        checkpoint("c1", None, &c1),
        checkpoint("c2", Some("c1"), &c2),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, b)| ev(i as u64 + 1, b))
    .collect();
    let mut chunks = BTreeMap::new();
    if content {
        for m in [&c1, &c2] {
            let cbor = m.to_cbor().unwrap();
            chunks.insert(*blake3::hash(&cbor).as_bytes(), cbor);
        }
        // Every file's content except the secret one (withheld).
        for data in [
            &b"let total = 0;\n"[..],
            b"let total = 1;\n",
            b"same\n",
            b"export {};\n",
        ] {
            chunks.insert(*blake3::hash(data).as_bytes(), data.to_vec());
        }
    }
    let meta = SessionMeta {
        id: "01jwasm".into(),
        name: Some("cart".into()),
        agent: "fakeagent".into(),
        mode: CaptureMode::Cell,
        status: SessionState::Closed,
        created_ms: 1_700_000_000_000,
    };
    let bytes = export(
        &ExportInput {
            session: &meta,
            events: &events,
            chunks: &chunks,
            content,
            withheld_files: u64::from(content),
            redaction: Redaction::Standard,
            divergences: |_| Ok(json!([])),
        },
        &SigningKey::from_bytes(&[3; 32]),
    )
    .unwrap()
    .bytes;
    (bytes, c1, c2)
}

fn hash(m: &Manifest) -> String {
    ChunkHash::of(&m.to_cbor().unwrap()).to_string()
}

#[test]
fn a_good_bundle_reports_every_check_and_exposes_its_events() {
    let (bytes, _, _) = bundle(true);
    let (report, opened) = open(&bytes);
    assert_eq!(report["ok"], true);
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 5);
    assert!(checks.iter().all(|c| c["status"] == "passed"), "{report}");
    assert_eq!(report["bundle"]["session"]["id"], "01jwasm");
    assert_eq!(report["bundle"]["events"], 3);
    assert!(report["error"].is_null());
    let events: Vec<Value> = serde_json::from_str(&opened.unwrap().events_json().unwrap()).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["body"]["kind"], "meta.session.started");
}

#[test]
fn a_tampered_bundle_names_the_failing_check_and_exposes_nothing() {
    let (mut bytes, _, _) = bundle(true);
    let n = bytes.len();
    bytes[n / 2] ^= 0x40;
    let (report, opened) = open(&bytes);
    assert!(opened.is_none());
    assert_eq!(report["ok"], false);
    assert!(report["bundle"].is_null());
    let failed: Vec<&Value> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "failed")
        .collect();
    assert_eq!(failed.len(), 1, "{report}");
    assert_eq!(failed[0]["stage"], report["error"]["stage"]);
    assert!(report["error"]["message"].as_str().unwrap().len() > 3);

    let (report, opened) = open(b"not a bundle at all");
    assert!(opened.is_none());
    assert_eq!(report["error"]["stage"], "archive");
}

#[test]
fn diffs_equal_the_cells() {
    let (bytes, c1, c2) = bundle(true);
    let (_, opened) = open(&bytes);
    let opened = opened.unwrap();
    let got: Value =
        serde_json::from_str(&opened.diff_json(&hash(&c1), &hash(&c2)).unwrap()).unwrap();
    let want = json!({ "changes": wyrd_cell::diff::diff(&c1, &c2) });
    assert_eq!(got, want);
    assert_eq!(got["changes"].as_array().unwrap().len(), 5);
    let back: Value =
        serde_json::from_str(&opened.diff_json(&hash(&c2), &hash(&c1)).unwrap()).unwrap();
    assert_eq!(back, json!({ "changes": wyrd_cell::diff::diff(&c2, &c1) }));
}

#[test]
fn file_sides_come_from_the_bundled_content() {
    let (bytes, c1, c2) = bundle(true);
    let (_, opened) = open(&bytes);
    let opened = opened.unwrap();
    let (a, b) = (hash(&c1), hash(&c2));
    let f = |path: &str| -> Value {
        serde_json::from_str(&opened.file_json(&a, &b, path).unwrap()).unwrap()
    };
    let cart = f("src/cart.ts");
    assert_eq!(cart["path"], "src/cart.ts");
    assert_eq!(cart["before"]["text"], "let total = 0;\n");
    assert_eq!(cart["after"]["text"], "let total = 1;\n");
    assert_eq!(cart["after"]["source"], "checkpoint");
    let new = f("src/new.ts");
    assert_eq!(new["before"]["source"], "workspace");
    assert!(new["before"]["text"].is_null());
    assert_eq!(new["after"]["text"], "export {};\n");
    assert_eq!(f("README.md")["after"]["source"], "absent");
    assert_eq!(f("src/link")["after"]["text"], "cart.ts");
    let secret = f("src/.env");
    assert!(secret["after"]["text"].is_null());
    assert_eq!(secret["after"]["withheld"], true);
    assert!(!secret.to_string().contains("withheld secret content"));
    for bad in ["", "/etc/passwd", "../x", "src//cart.ts", "src/./cart.ts"] {
        assert!(opened.file_json(&a, &b, bad).is_err(), "{bad}");
    }
    assert!(opened.diff_json("b3:00", &b).is_err());
    let missing = format!("b3:{}", "cd".repeat(32));
    assert!(
        opened
            .diff_json(&missing, &b)
            .unwrap_err()
            .contains("no checkpoint content")
    );
}

#[test]
fn a_bundle_without_content_has_events_but_no_diffs() {
    let (bytes, c1, c2) = bundle(false);
    let (report, opened) = open(&bytes);
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["bundle"]["content"]["included"], false);
    let opened = opened.unwrap();
    assert!(opened.events_json().is_ok());
    let err = opened.diff_json(&hash(&c1), &hash(&c2)).unwrap_err();
    assert!(err.contains("--no-content"), "{err}");
}
