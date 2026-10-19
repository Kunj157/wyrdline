//! Write the fuzz seed corpora (`fuzz/seeds/<target>/`) from real
//! exports: `cargo run -p wyrd-bundle --example fuzz_seeds -- fuzz/seeds`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use wyrd_bundle::read::{Options, parse_record, read};
use wyrd_bundle::{ExportInput, Redaction, SessionMeta, export};
use wyrd_schema::{
    CaptureMode, CheckpointCreated, DnsQuery, Event, EventBody, FilePath, FileWrite, IntentSource,
    Message, Role, SessionStarted, SessionState, ToolCallStarted,
};

fn events() -> Vec<Event> {
    let bodies = vec![
        EventBody::SessionStarted(SessionStarted {
            session_id: "01jseed".into(),
            agent_kind: "fakeagent".into(),
            agent_version: Some("1.0".into()),
            command: vec!["agent".into(), "fix the cart".into()],
            workspace_root: "/home/dev/shop".into(),
            mode: CaptureMode::Cell,
            kernel_release: "6.8.0".into(),
            capabilities: BTreeMap::from([("btf".into(), true)]),
            policy_set_hash: None,
        }),
        EventBody::ToolCallStarted(ToolCallStarted {
            call_id: "t1".into(),
            source: IntentSource::Hook,
            tool: "Bash".into(),
            args: serde_json::json!({"command": "curl https://api.example.org/x", "n": [1, 2.5, null]}),
            turn: Some(1),
        }),
        EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: "src/cart.ts".into(),
                in_cell: true,
                truncated: false,
            },
            bytes: 10,
        }),
        EventBody::DnsQuery(DnsQuery {
            name: "api.example.org".into(),
        }),
        EventBody::CheckpointCreated(CheckpointCreated {
            checkpoint_id: "c1".into(),
            branch_id: "a".into(),
            parent_id: None,
            after_call_id: Some("t1".into()),
            manifest: format!("b3:{}", "ab".repeat(32)),
            files_changed: 1,
            bytes_added: 10,
            duration_ms: 1,
        }),
        EventBody::Message(Message {
            role: Role::Assistant,
            turn: 1,
            text: "All tests pass.".into(),
        }),
    ];
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| Event {
            seq: i as u64 + 1,
            ts_mono_ns: 1000 * (i as u64 + 1),
            ts_wall_ms: 1_700_000_000_000,
            process: None,
            drops_since_last: 0,
            body,
        })
        .collect()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: fuzz_seeds <seeds dir>");
    let out = Path::new(&out);
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let meta = SessionMeta {
        id: "01jseed".into(),
        name: Some("seed".into()),
        agent: "fakeagent".into(),
        mode: CaptureMode::Cell,
        status: SessionState::Closed,
        created_ms: 1_700_000_000_000,
    };
    let events = events();
    let chunks: BTreeMap<[u8; 32], Vec<u8>> = [b"export const total = 1;\n".to_vec(), vec![0; 700]]
        .into_iter()
        .map(|b| (*blake3::hash(&b).as_bytes(), b))
        .collect();
    let none = BTreeMap::new();
    for (name, redaction, chunks) in [
        ("standard-content.bin", Redaction::Standard, &chunks),
        ("strict-no-content.bin", Redaction::Strict, &none),
    ] {
        let bundle = export(
            &ExportInput {
                session: &meta,
                events: &events,
                chunks,
                content: !chunks.is_empty(),
                withheld_files: 0,
                redaction,
                divergences: wyrd_divergence::view::project_json,
            },
            &key,
        )
        .unwrap()
        .bytes;
        write(&out.join("bundle_reader"), name, &bundle);
        let b = read(&bundle[..], &Options::default()).unwrap();
        let mut manifest = b.public_key.to_vec();
        manifest.extend_from_slice(&serde_json::to_vec_pretty(&b.manifest).unwrap());
        manifest.push(b'\n');
        write(&out.join("manifest"), name, &manifest);
    }
    let bundle = std::fs::read(out.join("bundle_reader/standard-content.bin")).unwrap();
    let b = read(&bundle[..], &Options::default()).unwrap();
    let mut ledger = Vec::new();
    for e in &b.events {
        let cbor = wyrd_schema::to_canonical_cbor(e).unwrap();
        write(
            &out.join("event_cbor"),
            &format!("event-{}.bin", e.seq),
            &cbor,
        );
        let hash = [0u8; 32];
        ledger.push(0x82);
        ciborium::into_writer(&ciborium::Value::Bytes(cbor), &mut ledger).unwrap();
        ciborium::into_writer(&ciborium::Value::Bytes(hash.to_vec()), &mut ledger).unwrap();
    }
    assert!(parse_record(&ledger).is_ok());
    write(&out.join("event_cbor"), "records.bin", &ledger);
}
