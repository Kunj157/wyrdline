#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Read commands: JSON on every one, golden human output, `tail`,
//! `token rotate`, and `daemon status` without a daemon.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

struct Env {
    tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

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
            command: vec!["agent".into(), "--task".into(), "demo".into()],
            observe: false,
            name: Some("golden".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    std::fs::write(s.dir().upper("a").join("a.txt"), "edited\n").unwrap();
    s.checkpoint(None).unwrap();
    std::fs::write(s.dir().upper("a").join("b.txt"), "new\n").unwrap();
    s.checkpoint(None).unwrap();
    let id = s.record().id.clone();
    wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(wyrd_correlator::synth::session(3).events)
        .unwrap();
    Env { tmp, data, id }
}

/// Append more synthetic events, so `wyrd events` prints more than a pipe
/// buffer holds (64 KiB).
fn grow_ledger(e: &Env) {
    let session = Session::open(&e.data, &e.id).unwrap();
    let mut ledger = wyrd_ledger::Ledger::open(session.dir().ledger()).unwrap();
    for seed in 4..16 {
        ledger
            .append(wyrd_correlator::synth::session(seed).events)
            .unwrap();
    }
}

fn wyrd(e: &Env) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wyrd"));
    c.env("WYRD_DATA_DIR", e.data.root())
        .env("XDG_STATE_HOME", e.tmp.path().join("state"))
        .env("WYRD_DAEMON_SOCKET", e.tmp.path().join("no-daemon.sock"));
    c
}

fn run(e: &Env, args: &[&str]) -> (i32, String) {
    let out = wyrd(e).args(args).output().unwrap();
    assert!(
        out.stderr.is_empty() || out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
    )
}

/// Stable text: ids, hashes, times, and temp paths replaced.
fn settings(e: &Env) -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.add_filter(&regex_escape(&e.tmp.path().display().to_string()), "[TMP]");
    s.add_filter(&e.id, "[SID]");
    s.add_filter(r"b3:[0-9a-f]{64}", "b3:[HASH]");
    s.set_prepend_module_to_snapshot(false);
    s
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-' {
                vec![c]
            } else {
                vec!['\\', c]
            }
        })
        .collect()
}

fn first_effect(e: &Env) -> u64 {
    let (_, g) = run(e, &["graph", &e.id, "--json"]);
    let g: serde_json::Value = serde_json::from_str(&g).unwrap();
    g["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["call_id"].is_string())
        .unwrap()["seq"]
        .as_u64()
        .unwrap()
}

#[test]
fn every_read_command_speaks_json() {
    let e = env();
    let seq = first_effect(&e).to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec!["sessions", "--json"],
        vec!["ls", "--json"],
        vec!["show", &e.id, "--json"],
        vec!["graph", &e.id, "--json"],
        vec!["explain", &e.id, &seq, "--json"],
        vec!["checkpoints", &e.id, "--json"],
        vec!["diff", &e.id, "c1", "c2", "--json"],
        vec!["verify", &e.id, "--json"],
        vec!["doctor", "--json"],
    ];
    for args in cases {
        let (code, out) = run(&e, &args);
        assert!(code == 0 || args[0] == "doctor", "{args:?} exit {code}");
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap_or_else(|err| panic!("{args:?}: {err}\n{out}"));
    }
    let (_, lines) = run(&e, &["events", &e.id, "--json"]);
    for line in lines.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
    let (_, ex) = run(&e, &["explain", &e.id, &seq, "--json"]);
    let ex: serde_json::Value = serde_json::from_str(&ex).unwrap();
    assert_eq!(ex["effect"]["seq"].as_u64().unwrap().to_string(), seq);
    assert!(ex["call"]["call_id"].is_string());
    assert_ne!(ex["process_chain"].as_array().unwrap().len(), 0);
}

#[test]
fn golden_human_output() {
    let e = env();
    let seq = first_effect(&e).to_string();
    settings(&e).bind(|| {
        insta::assert_snapshot!("show", run(&e, &["show", &e.id]).1);
        insta::assert_snapshot!("checkpoints", run(&e, &["checkpoints", &e.id]).1);
        insta::assert_snapshot!("explain", run(&e, &["explain", &e.id, &seq]).1);
    });
}

#[test]
fn verify_takes_a_session_id_or_a_file() {
    let e = env();
    let (code, out) = run(&e, &["verify", &e.id]);
    assert_eq!(code, 0, "{out}");
    let s = Session::open(&e.data, &e.id).unwrap();
    let (code, _) = run(&e, &["verify", &s.dir().ledger().display().to_string()]);
    assert_eq!(code, 0);
}

#[test]
fn tail_follows_new_events_until_the_session_closes() {
    let e = env();
    let mut child = wyrd(&e)
        .args(["tail", &e.id, "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let s = Session::open(&e.data, &e.id).unwrap();
    let before = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
        .unwrap()
        .head()
        .0;
    std::thread::sleep(Duration::from_millis(400));
    wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(wyrd_correlator::synth::session(4).events)
        .unwrap();
    Session::open(&e.data, &e.id)
        .unwrap()
        .close("test")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "tail did not exit after close");
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let seqs: Vec<u64> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs.first(), Some(&(before + 1)), "only new events");
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
    let head = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
        .unwrap()
        .head()
        .0;
    assert_eq!(seqs.last(), Some(&head), "through the seal");
}

#[test]
fn token_rotate_replaces_the_private_token_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let e = env();
    let (_, path) = run(&e, &["token", "path"]);
    let path = Path::new(path.trim()).to_path_buf();
    assert!(path.starts_with(e.tmp.path()));
    run(&e, &["token", "rotate"]);
    let first = std::fs::read_to_string(&path).unwrap();
    run(&e, &["token", "rotate"]);
    let second = std::fs::read_to_string(&path).unwrap();
    assert_ne!(first, second);
    assert_eq!(second.len(), 64);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (_, printed) = run(&e, &["token", "rotate"]);
    assert!(
        !printed.contains(&std::fs::read_to_string(&path).unwrap()),
        "never printed"
    );
}

#[test]
fn daemon_status_exits_5_without_a_daemon() {
    let e = env();
    let (code, out) = run(&e, &["daemon", "status", "--json"]);
    assert_eq!(code, 5);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["running"], false);
}

#[test]
fn tail_waits_for_the_closing_event_after_the_record_reads_closed() {
    // `Session::close` saves the record as Closed before it appends the
    // Closed state change, so a reader can see Closed while the last event is
    // still to come. Build that window: set the record to closed and hold the
    // session lock, as close does, for three seconds before appending the
    // closing event. Tail must wait for the lock, print that event, and exit
    // after it. It must not exit on the record alone, nor after a fixed grace.
    use wyrd_schema::{Event, EventBody, SessionState, SessionStateChanged};
    let e = env();
    let s = Session::open(&e.data, &e.id).unwrap();
    let state = s.dir().state();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    record["status"] = "closed".into();
    std::fs::write(&state, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    // `close` holds the session lock until the closing change is appended.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(s.dir().lock())
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let child = wyrd(&e)
        .args(["tail", &e.id, "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let closing = Event {
        seq: 0,
        ts_mono_ns: 0,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body: EventBody::SessionStateChanged(SessionStateChanged {
            from: SessionState::Closing,
            to: SessionState::Closed,
            reason: None,
        }),
    };
    let head = wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(vec![closing])
        .unwrap();
    drop(lock);

    let out = child.wait_with_output().unwrap();
    let last = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .last()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        });
    assert_eq!(
        last,
        Some(head),
        "the closing event is printed before tail exits"
    );
}

#[test]
fn tail_ends_a_session_whose_closing_event_never_came() {
    // A crash between saving the record as Closed and appending the change
    // leaves a closed record with no closing event. Tail must still finish, after
    // its grace period, rather than follow the session forever.
    let e = env();
    let s = Session::open(&e.data, &e.id).unwrap();
    let state = s.dir().state();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    record["status"] = "closed".into();
    std::fs::write(&state, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let mut child = wyrd(&e)
        .args(["tail", &e.id, "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "tail followed a closed session forever"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
}

/// Stress for the closing race: many sessions, each closed while `wyrd tail`
/// follows it, with the close varied against tail's poll. Counts sessions whose
/// final event tail did not print. Ignored: run with `--run-ignored all`.
#[test]
#[ignore = "stress: run with --run-ignored all"]
fn tail_stress_closing_race() {
    let rounds: u64 = std::env::var("TAIL_STRESS_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let mut missed = 0;
    for i in 0..rounds {
        let e = env();
        let mut child = wyrd(&e)
            .args(["tail", &e.id, "--json"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Vary the close against tail's 250 ms poll.
        std::thread::sleep(Duration::from_millis((i * 97) % 500));
        let mut s = Session::open(&e.data, &e.id).unwrap();
        s.close("stress").unwrap();
        let head = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())
            .unwrap()
            .head()
            .0;
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "round {i}: tail did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        let last = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .last()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                    .as_u64()
                    .unwrap()
            });
        if last != Some(head) {
            missed += 1;
            eprintln!("round {i}: tail printed through {last:?}, head {head}");
        }
    }
    eprintln!("tail stress: {missed} of {rounds} rounds missed the final event");
    assert_eq!(missed, 0);
}

fn state_change(to: wyrd_schema::SessionState) -> wyrd_schema::Event {
    use wyrd_schema::{Event, EventBody, SessionState, SessionStateChanged};
    let from = if to == SessionState::Closed {
        SessionState::Closing
    } else {
        SessionState::Recording
    };
    Event {
        seq: 0,
        ts_mono_ns: 0,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body: EventBody::SessionStateChanged(SessionStateChanged {
            from,
            to,
            reason: None,
        }),
    }
}

#[test]
fn tail_waits_for_a_close_after_an_earlier_failure() {
    // A session that failed earlier has a change to Failed in its ledger. If it
    // is then closed, the record reads closed while that old change is still the
    // latest one before the change to Closed. Tail must not stop on the old
    // change: it must wait for the lock, then print the change to Closed.
    use wyrd_schema::SessionState;
    let e = env();
    let s = Session::open(&e.data, &e.id).unwrap();
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    ledger
        .append(vec![state_change(SessionState::Failed)])
        .unwrap();
    drop(ledger);
    let state = s.dir().state();
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    record["status"] = "closed".into();
    std::fs::write(&state, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(s.dir().lock())
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let child = wyrd(&e)
        .args(["tail", &e.id, "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let head = wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(vec![state_change(SessionState::Closed)])
        .unwrap();
    drop(lock);

    let out = child.wait_with_output().unwrap();
    let last = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .last()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        });
    assert_eq!(
        last,
        Some(head),
        "the change to Closed is printed before tail exits"
    );
}

#[test]
fn a_reader_that_closes_early_gets_a_quiet_status_141() {
    // `wyrd events … | head -n1`: the reader takes one line and goes away. The
    // output is larger than a pipe buffer, so wyrd is still writing when the
    // reader closes and gets EPIPE. That is not an error: it exits quietly
    // with 141 (128 + SIGPIPE), the status a shell reports for a filter that
    // was stopped by the pipe, and prints nothing.
    use std::io::Read as _;
    let e = env();
    grow_ledger(&e);
    let full = wyrd(&e).args(["events", &e.id]).output().unwrap();
    assert!(
        full.stdout.len() > 65_536,
        "the output must not fit a pipe buffer ({} bytes)",
        full.stdout.len()
    );

    let mut child = wyrd(&e)
        .args(["events", &e.id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut first = [0u8; 1];
    stdout.read_exact(&mut first).unwrap();
    drop(stdout);
    let status = child.wait().unwrap();
    let mut err = String::new();
    stderr.read_to_string(&mut err).unwrap();
    assert_eq!(status.code(), Some(141), "stderr: {err}");
    assert!(err.is_empty(), "nothing is printed: {err}");
}

#[test]
fn a_closed_pipe_shared_by_both_streams_still_exits_141() {
    // `wyrd events … 2>&1 | head -n1`: stdout and stderr are one pipe. Once the
    // reader is gone, stderr is closed too, so an error report written with
    // eprintln! would panic and exit 101. Status 141 shows nothing was printed.
    use std::io::Read as _;
    let e = env();
    grow_ledger(&e);
    let (mut reader, writer) = std::io::pipe().unwrap();
    let mut cmd = wyrd(&e);
    cmd.args(["events", &e.id])
        .stdout(writer.try_clone().unwrap())
        .stderr(writer);
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    let mut first = [0u8; 1];
    reader.read_exact(&mut first).unwrap();
    drop(reader);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(141), "{status:?}");
}
