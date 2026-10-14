#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Full-pipeline scenarios driven by `wyrd-fakeagent` (plan §21.4).
//!
//! Each scenario runs a `demo/scenarios/*.wyrdscript.toml` under the real
//! `wyrd run` and asserts on the sealed ledger. The `wyrd` binary is taken
//! from the same target directory; `make test-scenarios` builds it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use wyrd_schema::{CallStatus, EventBody, IntentSource, Role};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fakeagent() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_wyrd-fakeagent"))
}

fn wyrd_bin() -> PathBuf {
    let p = fakeagent().with_file_name("wyrd");
    assert!(
        p.exists(),
        "{} is missing: build the workspace first (make test-scenarios)",
        p.display()
    );
    p
}

struct Scenario {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    data: PathBuf,
    secret: String,
}

impl Scenario {
    fn new() -> Self {
        let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let ws = tmp.path().join("ws");
        let data = tmp.path().join("data");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("greet.txt"), "helo world\n").unwrap();
        fs::write(ws.join("old.txt"), "stale\n").unwrap();
        // Planted at runtime so no secret-shaped literal is in the repository.
        let secret = format!("{}{}", "ghp_", "Zq7Lm2Xv9Rt4Wb8Nc1Kd5Hf3Gj6Ps0Ty2Ua4E");
        Scenario {
            _tmp: tmp,
            ws,
            data,
            secret,
        }
    }

    fn run(&self, script: &str, extra: &[&str], privileged: bool) -> Output {
        let script = repo().join("demo/scenarios").join(script);
        let mut args: Vec<String> = vec!["run".into(), "--agent".into(), "fakeagent".into()];
        if !privileged {
            args.push("--observe".into());
        }
        args.extend(extra.iter().map(|s| (*s).to_string()));
        args.extend([
            "--".into(),
            fakeagent().display().to_string(),
            "run".into(),
            script.display().to_string(),
        ]);
        let mut cmd = Command::new(wyrd_bin());
        cmd.args(&args)
            .current_dir(&self.ws)
            .env("WYRD_DATA_DIR", &self.data)
            .env("WYRD_SCENARIO_SECRET", &self.secret);
        if privileged {
            cmd.env("WYRD_PRIVILEGED_HELPER", "sudo");
        }
        let out = cmd.output().unwrap();
        eprintln!(
            "wyrd {args:?} -> {:?}\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn session_dir(&self) -> PathBuf {
        fs::read_dir(self.data.join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    fn ledger(&self) -> PathBuf {
        self.session_dir().join("ledger.wyrdledger")
    }

    fn events(&self) -> Vec<EventBody> {
        wyrd_ledger::Ledger::open_read_only(self.ledger())
            .unwrap()
            .events_after(0, usize::MAX)
            .unwrap()
            .into_iter()
            .map(|e| e.body)
            .collect()
    }

    fn close_and_verify(&self) {
        let sid = self
            .session_dir()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let out = Command::new(wyrd_bin())
            .args(["close", &sid])
            .env("WYRD_DATA_DIR", &self.data)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new(wyrd_bin())
            .args(["verify", self.ledger().to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// The raw ledger file, every byte, must not contain the planted secret.
    fn assert_secret_absent(&self) {
        let mut raw = fs::read(self.ledger()).unwrap();
        // WAL content counts too.
        if let Ok(wal) = fs::read(self.ledger().with_extension("wyrdledger-wal")) {
            raw.extend(wal);
        }
        let needle = self.secret.as_bytes();
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "planted secret leaked into the ledger"
        );
    }
}

/// Calls the script makes, in order, with whether the test expects them to
/// finish (denied ones never start executing).
const CALLS: &[(&str, &str, Option<CallStatus>)] = &[
    ("fa-1-1", "Read", Some(CallStatus::Ok)),
    ("fa-1-2", "Edit", Some(CallStatus::Ok)),
    ("fa-1-3", "Bash", Some(CallStatus::Ok)),
    ("fa-1-4", "Bash", Some(CallStatus::Ok)),
    ("fa-1-5", "Bash", Some(CallStatus::Error)),
    ("fa-1-6", "mcp__toy__echo", Some(CallStatus::Ok)),
    ("fa-1-8", "WebFetch", None),
    ("fa-1-9", "Write", Some(CallStatus::Ok)),
];

fn assert_intent(events: &[EventBody]) {
    for (id, tool, status) in CALLS {
        let started = events.iter().find_map(|e| match e {
            EventBody::ToolCallStarted(s) if s.call_id == *id => Some(s),
            _ => None,
        });
        let started = started.unwrap_or_else(|| panic!("{id} never started"));
        assert_eq!(started.tool, *tool);
        assert_eq!(started.source, IntentSource::Hook);
        let finished = events.iter().find_map(|e| match e {
            EventBody::ToolCallFinished(f) if f.call_id == *id => Some(f.status),
            _ => None,
        });
        assert_eq!(finished, *status, "{id}");
    }
    let denied: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            EventBody::ToolCallDenied(d) => Some(d.call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(denied, ["fa-1-8"]);
    // The MCP call is also seen by the gateway, with its own id.
    assert!(events.iter().any(|e| matches!(e, EventBody::ToolCallStarted(s) if s.source == IntentSource::Mcp && s.tool == "mcp__toy__echo")));
    assert!(
        events.iter().any(
            |e| matches!(e, EventBody::GuardDecision(g) if g.policy_id == "wyrdline.mcp-pinning")
        ),
        "rug pull flagged"
    );
    let exit2 = events.iter().find_map(|e| match e {
        EventBody::ToolCallFinished(f) if f.call_id == "fa-1-5" => f.exit_code,
        _ => None,
    });
    assert_eq!(exit2, Some(2));
    let messages: Vec<(Role, u32, &str)> = events
        .iter()
        .filter_map(|e| match e {
            EventBody::Message(m) => Some((m.role, m.turn, m.text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages[1],
        (Role::Assistant, 1, "The greeting is misspelled; fixing it.")
    );
    assert_eq!(messages[4], (Role::Assistant, 2, "You're welcome."));
    assert!(
        messages[0].2.contains("[REDACTED:"),
        "the prompt's token is redacted"
    );
}

#[test]
fn scenario_basic_observe_records_every_call_and_message() {
    let s = Scenario::new();
    let out = s.run("basic.wyrdscript.toml", &["--deny-tool", "WebFetch"], false);
    assert_eq!(out.status.code(), Some(0));
    let events = s.events();
    assert_intent(&events);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EventBody::CheckpointCreated(_))),
        "observe mode has no checkpoints"
    );
    // Observe mode writes to the real workspace.
    assert_eq!(
        fs::read_to_string(s.ws.join("greet.txt")).unwrap(),
        "hello world\n"
    );
    s.close_and_verify();
    s.assert_secret_absent();
}

#[cfg(feature = "privileged-tests")]
#[test]
fn privileged_scenario_basic_cell_checkpoints_every_finished_call() {
    let s = Scenario::new();
    let out = s.run("basic.wyrdscript.toml", &["--deny-tool", "WebFetch"], true);
    assert_eq!(out.status.code(), Some(0));
    let events = s.events();
    assert_intent(&events);
    // Every finished call is followed by a checkpoint before the next call
    // starts. An MCP call finishes twice (gateway, then the agent's hook)
    // within the debounce window; one checkpoint covers both and is
    // attributed to the agent's call.
    for (i, e) in events.iter().enumerate() {
        let EventBody::ToolCallFinished(f) = e else {
            continue;
        };
        let next = events[i + 1..].iter().find(|x| {
            matches!(
                x,
                EventBody::CheckpointCreated(_) | EventBody::ToolCallStarted(_)
            )
        });
        let Some(EventBody::CheckpointCreated(c)) = next else {
            panic!(
                "{} finished without a checkpoint (next: {next:?})",
                f.call_id
            )
        };
        if f.call_id.starts_with("fa-") {
            assert_eq!(c.after_call_id.as_deref(), Some(f.call_id.as_str()));
        }
    }
    let checkpoints = events
        .iter()
        .filter(|e| matches!(e, EventBody::CheckpointCreated(_)))
        .count();
    assert_eq!(checkpoints, 7, "one per finished agent call");
    // The Cell kept the real workspace untouched.
    assert_eq!(
        fs::read_to_string(s.ws.join("greet.txt")).unwrap(),
        "helo world\n"
    );
    assert!(s.ws.join("old.txt").exists());
    s.close_and_verify();
    s.assert_secret_absent();
}
