#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd export` against real (unprivileged) sessions: checkpoints made
//! from a Cell upper directory, a sealed ledger, and planted secrets.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{DnsQuery, Event, EventBody, FilePath, FileWrite, SessionState};

struct Env {
    tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
    /// Content of the `.env` file the agent wrote into the Cell.
    dotenv: String,
}

fn ev(body: EventBody) -> Event {
    Event {
        seq: 0,
        ts_mono_ns: 1,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body,
    }
}

/// A session with two checkpoints (one holds a `.env`) and events that
/// mention a home directory and a host, then closed and sealed.
fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: false,
            name: Some("bundle".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    let upper = s.dir().upper("a");
    std::fs::write(upper.join("a.txt"), "edited\n").unwrap();
    s.checkpoint(None).unwrap();
    let dotenv = format!("DB_URL=postgres://u:{}@db/x", "pw-planted-9");
    std::fs::write(upper.join(".env"), &dotenv).unwrap();
    s.checkpoint(None).unwrap();
    wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(vec![
            ev(EventBody::FileWrite(FileWrite {
                file: FilePath {
                    path: "/home/bob-planted/shop/src/cart.ts".into(),
                    in_cell: true,
                    truncated: false,
                },
                bytes: 3,
            })),
            ev(EventBody::DnsQuery(DnsQuery {
                name: "exfil.planted.example".into(),
            })),
        ])
        .unwrap();
    s.close("test").unwrap();
    let id = s.record().id.clone();
    Env {
        tmp,
        data,
        id,
        dotenv,
    }
}

fn wyrd(e: &Env, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(args)
        .env("WYRD_DATA_DIR", e.data.root())
        .env("XDG_STATE_HOME", e.tmp.path().join("state"))
        .env("WYRD_DAEMON_SOCKET", e.tmp.path().join("no-daemon.sock"))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn out_path(e: &Env, name: &str) -> PathBuf {
    e.tmp.path().join(name)
}

fn export(e: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let path = out_path(e, name);
    let p = path.display().to_string();
    let mut args = vec!["export", &e.id, "-o", &p];
    args.extend_from_slice(extra);
    let (code, stdout, stderr) = wyrd(e, &args);
    assert_eq!(code, 0, "{stdout}{stderr}");
    path
}

/// Archive entries of a bundle (name, bytes).
fn entries(bundle: &Path) -> Vec<(String, Vec<u8>)> {
    let bytes = std::fs::read(bundle).unwrap();
    let mut tar = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(&bytes[..])
        .unwrap()
        .read_to_end(&mut tar)
        .unwrap();
    tar::Archive::new(&tar[..])
        .entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            (e.path().unwrap().display().to_string(), data)
        })
        .collect()
}

fn contains(entries: &[(String, Vec<u8>)], needle: &str) -> bool {
    entries
        .iter()
        .any(|(_, d)| d.windows(needle.len()).any(|w| w == needle.as_bytes()))
}

#[test]
fn export_prints_counts_and_the_manifest_hash() {
    let e = env();
    let path = out_path(&e, "s.wyrd");
    let (code, stdout, _) = wyrd(&e, &["export", &e.id, "-o", &path.display().to_string()]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("redaction strict"), "{stdout}");
    assert!(stdout.contains("manifest  b3:"), "{stdout}");
    assert!(
        stdout.contains("(1 secret-classified files withheld)"),
        "{stdout}"
    );
    let (_, json, _) = wyrd(
        &e,
        &[
            "export",
            &e.id,
            "-o",
            &out_path(&e, "j.wyrd").display().to_string(),
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["session"], e.id.as_str());
    assert_eq!(v["withheld_files"], 1);
    assert!(v["chunks"].as_u64().unwrap() >= 3);
    let names: Vec<String> = entries(&path).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names[..2], ["manifest.json", "signature.ed25519"]);
    assert_eq!(names.len() as u64, v["entries"].as_u64().unwrap());
}

#[test]
fn two_exports_of_one_session_are_identical() {
    let e = env();
    let a = export(&e, "a.wyrd", &[]);
    let b = export(&e, "b.wyrd", &[]);
    assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
}

#[test]
fn strict_hides_users_hosts_and_secret_files_standard_keeps_content() {
    let e = env();
    let strict = entries(&export(&e, "strict.wyrd", &["--redact", "strict"]));
    for needle in ["bob-planted", "exfil.planted.example", "pw-planted-9"] {
        assert!(!contains(&strict, needle), "{needle} in a strict bundle");
    }
    let standard = entries(&export(&e, "std.wyrd", &["--redact", "standard"]));
    assert!(contains(&standard, "bob-planted"));
    assert!(
        contains(&standard, &e.dotenv),
        "standard keeps file content"
    );
}

#[test]
fn no_content_leaves_out_cas() {
    let e = env();
    let names: Vec<String> = entries(&export(&e, "meta.wyrd", &["--no-content"]))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.iter().all(|n| !n.starts_with("cas/")), "{names:?}");
    assert!(names.contains(&"ledger.cbor-seq".to_string()));
}

#[test]
fn annotations_file_goes_into_standard_bundles_only() {
    let e = env();
    let doc = b"annotation document bytes".to_vec();
    let file = e.tmp.path().join("notes.automerge");
    std::fs::write(&file, &doc).unwrap();
    let file = file.display().to_string();
    let standard = entries(&export(
        &e,
        "std.wyrd",
        &["--redact", "standard", "--annotations", file.as_str()],
    ));
    let got = standard.iter().find(|(p, _)| p == "annotations.automerge");
    assert_eq!(got.map(|(_, b)| b), Some(&doc));
    let strict = entries(&export(
        &e,
        "strict.wyrd",
        &["--annotations", file.as_str()],
    ));
    assert!(!strict.iter().any(|(p, _)| p == "annotations.automerge"));
}

#[test]
fn unknown_session_exits_2_and_writes_nothing() {
    let e = env();
    let path = out_path(&e, "none.wyrd");
    let (code, _, stderr) = wyrd(&e, &["export", "zzzz", "-o", &path.display().to_string()]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("no session zzzz"), "{stderr}");
    assert!(!path.exists());
}

#[test]
fn a_recording_session_is_refused() {
    let e = env();
    let s = Session::open(&e.data, &e.id).unwrap();
    let mut rec = s.record().clone();
    rec.status = SessionState::Recording;
    wyrd_cell::state::save(s.dir(), &rec).unwrap();
    let path = out_path(&e, "live.wyrd");
    let (code, _, stderr) = wyrd(&e, &["export", &e.id, "-o", &path.display().to_string()]);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("still recording"), "{stderr}");
    assert!(!path.exists());
}

#[test]
fn a_failed_write_leaves_the_target_and_no_temporary_file() {
    let e = env();
    let target = out_path(&e, "taken.wyrd");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), "x").unwrap();
    let before: Vec<_> = std::fs::read_dir(e.tmp.path()).unwrap().collect();
    let (code, _, stderr) = wyrd(&e, &["export", &e.id, "-o", &target.display().to_string()]);
    assert_ne!(code, 0);
    assert!(stderr.contains("writing"), "{stderr}");
    assert!(target.join("keep").exists());
    let after: Vec<_> = std::fs::read_dir(e.tmp.path()).unwrap().collect();
    assert_eq!(
        before.len(),
        after.len(),
        "a temporary file was left behind"
    );
}

#[test]
fn a_tampered_ledger_is_not_exported() {
    let e = env();
    let ledger = e.data.session(&e.id).ledger();
    let l = wyrd_ledger::Ledger::open(&ledger).unwrap();
    l.conn()
        .execute_batch("DROP TRIGGER events_no_update;")
        .unwrap();
    l.conn()
        .execute("UPDATE events SET hash = zeroblob(32) WHERE seq = 2", [])
        .unwrap();
    drop(l);
    let path = out_path(&e, "bad.wyrd");
    let (code, _, stderr) = wyrd(&e, &["export", &e.id, "-o", &path.display().to_string()]);
    assert_eq!(code, 3, "{stderr}");
    assert!(!path.exists());
}

#[test]
fn verify_lists_every_check_for_a_good_bundle() {
    let e = env();
    let path = export(&e, "ok.wyrd", &["--redact", "standard"]);
    let p = path.display().to_string();
    let (code, stdout, stderr) = wyrd(&e, &["verify", &p]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    for line in [
        "✓ archive structure and limits",
        "✓ signature over the manifest",
        "✓ manifest",
        "✓ file hashes",
        "✓ ledger hash chain",
        "result    ✓ bundle verified",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }
    let (code, stdout, _) = wyrd(&e, &["verify", &p, "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["checks"].as_array().unwrap().len(), 5);
    assert_eq!(v["bundle"]["session"]["id"], e.id.as_str());
}

/// Rewrite the bundle at `path` with one byte of the ledger entry changed
/// inside the DNS query's name (same size); returns that event's seq.
fn tamper_dns_event(path: &Path) -> u64 {
    let bytes = std::fs::read(path).unwrap();
    let b = wyrd_bundle::read(&bytes[..], &wyrd_bundle::Options::default()).unwrap();
    let (seq, name) = b
        .events
        .iter()
        .find_map(|ev| match &ev.body {
            EventBody::DnsQuery(q) => Some((ev.seq, q.name.clone())),
            _ => None,
        })
        .unwrap();
    let mut all = entries(path);
    let ledger = &mut all
        .iter_mut()
        .find(|(n, _)| n == "ledger.cbor-seq")
        .unwrap()
        .1;
    let at = ledger
        .windows(name.len())
        .position(|w| w == name.as_bytes())
        .unwrap();
    ledger[at] = if ledger[at] == b'x' { b'y' } else { b'x' };
    let tar =
        wyrd_bundle::archive::pack(all.iter().map(|(n, d)| (n.as_str(), d.as_slice()))).unwrap();
    std::fs::write(path, wyrd_bundle::archive::compress(&tar)).unwrap();
    seq
}

#[test]
fn a_tampered_bundle_fails_verification_at_its_seq_and_is_not_imported() {
    let e = env();
    let path = export(&e, "bad.wyrd", &[]);
    let seq = tamper_dns_event(&path);
    let p = path.display().to_string();
    let (code, stdout, _) = wyrd(&e, &["verify", &p]);
    assert_eq!(code, 3, "{stdout}");
    assert!(
        stdout.contains(&format!("event modified at seq {seq}")),
        "{stdout}"
    );
    assert!(stdout.contains("✓ signature over the manifest"), "{stdout}");
    assert!(stdout.contains("✗ file hashes"), "{stdout}");
    let (_, stdout, _) = wyrd(&e, &["verify", &p, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["seq"], seq);
    assert_eq!(v["error"]["stage"], "files");

    let before = Session::list(&e.data).unwrap().len();
    let (code, _, _) = wyrd(&e, &["import", &p]);
    assert_eq!(code, 3);
    assert_eq!(Session::list(&e.data).unwrap().len(), before);
    let leftovers: Vec<_> = std::fs::read_dir(e.data.sessions())
        .unwrap()
        .flatten()
        .filter(|d| d.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn import_adds_a_read_only_session_with_the_same_chain() {
    let e = env();
    let path = export(&e, "in.wyrd", &["--redact", "standard"]);
    let (code, stdout, stderr) = wyrd(&e, &["import", &path.display().to_string(), "--json"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let id = v["session"].as_str().unwrap().to_string();
    assert_ne!(id, e.id);
    assert_eq!(v["original_id"], e.id.as_str());
    assert_eq!(v["checkpoints"], 2);

    // Same chain head as the bundle; the ledger verifies.
    let (code, stdout, _) = wyrd(&e, &["verify", &id, "--json"]);
    assert_eq!(code, 0, "{stdout}");
    let check: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(check["head_hash"], v["head_hash"]);

    let s = Session::open(&e.data, &id).unwrap();
    let src = s.imported().unwrap();
    assert_eq!(src.original_id, e.id);
    assert_eq!(src.redaction, "standard");
    assert_eq!(s.record().status, SessionState::Closed);

    // Content is there: the checkpoints diff.
    let (code, stdout, stderr) = wyrd(&e, &["diff", &id, "c1", "c2"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains(".env"), "{stdout}");

    // Nothing can change it.
    for args in [
        vec!["rewind", id.as_str(), "c1"],
        vec!["checkpoint", id.as_str()],
        vec!["close", id.as_str()],
    ] {
        let (code, stdout, stderr) = wyrd(&e, &args);
        assert_ne!(code, 0, "{args:?}: {stdout}");
        assert!(stderr.contains("read-only"), "{args:?}: {stderr}");
    }
    let mut s = Session::open(&e.data, &id).unwrap();
    assert!(s.apply("c2", true, false).is_err());
    assert!(s.record_verdicts(Vec::new()).is_err());
    assert!(s.request_rewind("c1", "agent asks", "mcp").is_err());
    assert!(s.reject_rewind(1, "cli", None).is_err());
    assert!(s.confirm_rewind(1, "cli").is_err());
}
