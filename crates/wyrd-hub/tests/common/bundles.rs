//! Signed test bundles built with `wyrd-bundle`'s exporter, and ways to
//! spoil them.

use std::collections::BTreeMap;
use std::io::Read as _;

use ed25519_dalek::SigningKey;
use wyrd_bundle::{ExportInput, Redaction, SessionMeta, archive, export};
use wyrd_schema::{
    CaptureMode, DivergenceRaised, Event, EventBody, FilePath, FileWrite, Message, Role,
    SessionStarted, SessionState, Severity,
};

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

/// A closed session `name` that raised the divergence `rules`.
pub fn bundle(name: &str, rules: &[&str]) -> Vec<u8> {
    let session = format!("01j{name}");
    let mut bodies = vec![
        EventBody::SessionStarted(SessionStarted {
            session_id: session.clone(),
            agent_kind: "fakeagent".into(),
            agent_version: None,
            command: vec!["agent".into()],
            workspace_root: "~/shop".into(),
            mode: CaptureMode::Cell,
            kernel_release: "6.8.0".into(),
            capabilities: BTreeMap::new(),
            policy_set_hash: None,
        }),
        EventBody::FileWrite(FileWrite {
            file: FilePath {
                path: "src/cart.ts".into(),
                in_cell: true,
                truncated: false,
            },
            bytes: 10,
        }),
        EventBody::Message(Message {
            role: Role::Assistant,
            turn: 1,
            text: "All tests pass.".into(),
        }),
    ];
    for (i, r) in rules.iter().enumerate() {
        bodies.push(EventBody::DivergenceRaised(DivergenceRaised {
            divergence_id: format!("d{i}"),
            rule_id: (*r).to_string(),
            severity: Severity::High,
            claim_id: None,
            evidence_seqs: vec![2],
            confidence_bp: 9000,
            summary: format!("{r} raised"),
        }));
    }
    let events: Vec<Event> = bodies
        .into_iter()
        .enumerate()
        .map(|(i, b)| ev(i as u64 + 1, b))
        .collect();
    let meta = SessionMeta {
        id: session,
        name: Some(name.into()),
        agent: "fakeagent".into(),
        mode: CaptureMode::Cell,
        status: SessionState::Closed,
        created_ms: 1_700_000_000_000,
    };
    export(
        &ExportInput {
            session: &meta,
            events: &events,
            chunks: &BTreeMap::new(),
            content: false,
            withheld_files: 0,
            redaction: Redaction::Standard,
            divergences: wyrd_divergence::view::project_json,
        },
        &SigningKey::from_bytes(&[7; 32]),
    )
    .unwrap()
    .bytes
}

/// The archive entries of a bundle, in order.
pub fn untar(bundle: &[u8]) -> Vec<(String, Vec<u8>)> {
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
            let mut body = Vec::new();
            e.read_to_end(&mut body).unwrap();
            (name, body)
        })
        .collect()
}

/// Repack entries (after a test changed them).
pub fn repack(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let tar = archive::pack(entries.iter().map(|(p, b)| (p.as_str(), b.as_slice()))).unwrap();
    archive::compress(&tar)
}

/// One byte of the ledger changed: the file hash check fails.
pub fn tampered(name: &str) -> Vec<u8> {
    let mut e = untar(&bundle(name, &[]));
    let ledger = e.iter_mut().find(|(p, _)| p == "ledger.cbor-seq").unwrap();
    let mid = ledger.1.len() / 2;
    ledger.1[mid] ^= 0x01;
    repack(&e)
}

/// The signature replaced by zeros: the signature check fails.
pub fn unsigned(name: &str) -> Vec<u8> {
    let mut e = untar(&bundle(name, &[]));
    let sig = e
        .iter_mut()
        .find(|(p, _)| p == "signature.ed25519")
        .unwrap();
    sig.1 = vec![0; 96];
    repack(&e)
}

/// A multipart body with the `bundle` file and optional `repo`.
pub fn multipart(bundle: &[u8], repo: Option<&str>) -> (String, Vec<u8>) {
    let boundary = format!("wyrd{}", super::throwaway());
    let mut body = Vec::new();
    if let Some(r) = repo {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"repo\"\r\n\r\n{r}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bundle\"; filename=\"s.wyrd\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bundle);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}
