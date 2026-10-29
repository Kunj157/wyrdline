#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd push` against a stand-in Hub (a one-request HTTP server): the
//! request it sends, and how answers map to exit codes. The real Hub's
//! side is covered by the wyrd-hub container tests.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;

use ed25519_dalek::SigningKey;
use wyrd_bundle::{ExportInput, Redaction, SessionMeta, export};
use wyrd_schema::{CaptureMode, Event, EventBody, Message, Role, SessionState};

fn bundle() -> Vec<u8> {
    let events = vec![Event {
        seq: 1,
        ts_mono_ns: 1,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body: EventBody::Message(Message {
            role: Role::Assistant,
            turn: 1,
            text: "hi".into(),
        }),
    }];
    let meta = SessionMeta {
        id: "01jpush".into(),
        name: Some("push".into()),
        agent: "fakeagent".into(),
        mode: CaptureMode::Cell,
        status: SessionState::Closed,
        created_ms: 0,
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
            annotations: None,
        },
        &SigningKey::from_bytes(&[3; 32]),
    )
    .unwrap()
    .bytes
}

struct Seen {
    head: String,
    body: Vec<u8>,
}

/// Serve one request with `status` and `body`; report what arrived.
fn hub(status: u16, answer: &'static str) -> (String, mpsc::Receiver<Seen>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let len: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_string)
            })
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        let mut s = s;
        write!(
            s,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
            answer.len()
        )
        .unwrap();
        tx.send(Seen { head, body }).unwrap();
    });
    (url, rx)
}

fn wyrd(args: &[&str], token: Option<&str>) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wyrd"));
    c.args(args)
        .env_remove("WYRD_HUB_URL")
        .env_remove("WYRD_HUB_TOKEN");
    if let Some(t) = token {
        c.env("WYRD_HUB_TOKEN", t);
    }
    c.output().unwrap()
}

#[test]
fn uploads_with_the_token_and_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.wyrd");
    let bytes = bundle();
    std::fs::write(&file, &bytes).unwrap();
    let (url, seen) = hub(201, r#"{"duplicate":false,"bundle":{"id":"01BUNDLE"}}"#);
    let out = wyrd(
        &[
            "push",
            file.to_str().unwrap(),
            "--hub",
            &url,
            "--org",
            "acme",
            "--repo",
            "o/r",
        ],
        Some("wyrdhub_test"),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("uploaded as 01BUNDLE"));
    let s = seen.recv().unwrap();
    assert!(
        s.head
            .starts_with("POST /api/v1/orgs/acme/bundles HTTP/1.1"),
        "{}",
        s.head
    );
    assert!(
        s.head
            .to_ascii_lowercase()
            .contains("authorization: bearer wyrdhub_test")
    );
    assert!(s.head.contains("multipart/form-data; boundary="));
    let body = s.body;
    assert!(
        body.windows(bytes.len()).any(|w| w == bytes.as_slice()),
        "the file is sent unchanged"
    );
    assert!(String::from_utf8_lossy(&body).contains("name=\"repo\"\r\n\r\no/r\r\n"));
}

#[test]
fn exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.wyrd");
    std::fs::write(&file, bundle()).unwrap();
    let f = file.to_str().unwrap();
    // No token: usage, nothing sent.
    let out = wyrd(
        &["push", f, "--hub", "http://127.0.0.1:9", "--org", "acme"],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    // The Hub refuses the bundle: verification failed.
    let (url, _seen) = hub(
        422,
        r#"{"title":"The bundle failed verification","detail":"x"}"#,
    );
    let out = wyrd(&["push", f, "--hub", &url, "--org", "acme"], Some("t"));
    assert_eq!(out.status.code(), Some(3));
    // Over quota.
    let (url, _seen) = hub(429, r#"{"title":"Bundle count quota reached"}"#);
    let out = wyrd(&["push", f, "--hub", &url, "--org", "acme"], Some("t"));
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("quota"));
    // Unreachable Hub.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let out = wyrd(&["push", f, "--hub", &url, "--org", "acme"], Some("t"));
    assert_eq!(out.status.code(), Some(5));
    // A corrupt local file is caught before any upload.
    std::fs::write(&file, b"garbage").unwrap();
    let out = wyrd(
        &["push", f, "--hub", "http://127.0.0.1:9", "--org", "acme"],
        Some("t"),
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not verify"));
}
