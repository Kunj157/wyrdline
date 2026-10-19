//! Host conformance against `policies/fixtures/hostile.wasm`, a guest that
//! misbehaves on request (source: `policies/examples/hostile`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use wyrd_policy::{
    Effect, FUEL_PER_CALL, LOG_LINE_BYTES, LOG_LINES, Policy, PolicyError, SessionCtx, ToolCall,
    Verdict,
};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../policies/fixtures/hostile.wasm")
}

fn hostile() -> Policy {
    Policy::load_file(&fixture()).expect("load hostile.wasm")
}

fn ctx() -> SessionCtx {
    SessionCtx {
        repo_root: "/work/shop".into(),
        scope_globs: vec!["src/**".into()],
        prior_denials: 0,
    }
}

fn call(tool: &str) -> ToolCall {
    ToolCall {
        tool: tool.into(),
        args_json: r#"{"command":"ls"}"#.into(),
        adapter: "claude-code".into(),
        cwd: "/work/shop".into(),
    }
}

#[test]
fn id_and_version_come_from_the_file() {
    let p = hostile();
    assert_eq!(p.id, "hostile");
    assert!(p.version.starts_with("b3:"));
    assert_eq!(p.version.len(), 3 + 16);
    let again = hostile();
    assert_eq!(p.version, again.version);
}

#[test]
fn decisions_pass_through() {
    let p = hostile();
    let o = p.on_tool_call(&call("Read"), &ctx());
    assert_eq!(o.value, Verdict::Allow);
    assert_eq!(o.error, None);
    assert_eq!(o.log, vec!["allow Read".to_string()]);
    assert!(o.fuel_used > 0 && o.fuel_used < FUEL_PER_CALL);

    let o = p.on_tool_call(&call("deny"), &ctx());
    assert_eq!(o.value, Verdict::Deny("denied in /work/shop".into()));

    let o = p.on_tool_call(&call("ask"), &ctx());
    assert_eq!(o.value, Verdict::Ask(r#"args {"command":"ls"}"#.into()));
}

#[test]
fn effects_are_decided() {
    let p = hostile();
    let curl = Effect {
        kind: "exec".into(),
        exe: Some("/usr/bin/curl".into()),
        ..Effect::default()
    };
    assert_eq!(
        p.on_effect(&curl, &ctx()).value,
        Verdict::Deny("curl".into())
    );
    let connect = Effect {
        kind: "net.connect".into(),
        host: Some("203.0.113.9".into()),
        port: Some(443),
        ..Effect::default()
    };
    assert_eq!(
        p.on_effect(&connect, &ctx()).value,
        Verdict::Ask("203.0.113.9:443".into())
    );
}

#[test]
fn kernel_rules_are_returned() {
    let k = hostile().kernel_rules(&ctx()).value.expect("rules");
    assert_eq!(k.fs_read_deny, vec!["~/.ssh"]);
    assert_eq!(k.fs_write_allow, vec!["/work/shop"]);
    assert_eq!(k.egress_allow_ports, vec![443]);
}

#[test]
fn infinite_loop_runs_out_of_fuel_and_denies() {
    let p = hostile();
    let t = Instant::now();
    let o = p.on_tool_call(&call("spin"), &ctx());
    assert_eq!(
        o.value,
        Verdict::Deny("policy error: fuel exhausted".into())
    );
    assert_eq!(o.error.as_deref(), Some("fuel exhausted"));
    assert_eq!(o.fuel_used, FUEL_PER_CALL);
    // Generous bound for slow CI runners; ~8 ms on the dev box.
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[test]
fn kernel_rules_failure_yields_no_rules() {
    let o = hostile().kernel_rules(&SessionCtx {
        repo_root: "spin".into(),
        ..SessionCtx::default()
    });
    assert_eq!(o.value, None);
    assert_eq!(o.error.as_deref(), Some("fuel exhausted"));
}

#[test]
fn huge_allocation_traps_and_denies() {
    let o = hostile().on_tool_call(&call("hog"), &ctx());
    let Verdict::Deny(reason) = &o.value else {
        panic!("expected deny, got {:?}", o.value);
    };
    assert!(reason.starts_with("policy error: "), "{reason}");
    assert!(o.error.is_some());
}

#[test]
fn panic_traps_and_denies() {
    let o = hostile().on_tool_call(&call("panic"), &ctx());
    assert!(matches!(&o.value, Verdict::Deny(r) if r.starts_with("policy error: ")));
}

#[test]
fn wasi_imports_trap() {
    // The guest asks for the wall clock: no clocks for policies.
    let o = hostile().on_tool_call(&call("clock"), &ctx());
    assert!(matches!(&o.value, Verdict::Deny(r) if r.starts_with("policy error: ")));
}

#[test]
fn log_output_is_bounded() {
    let o = hostile().on_tool_call(&call("flood"), &ctx());
    assert_eq!(o.value, Verdict::Allow);
    assert_eq!(o.log.len(), LOG_LINES);
    assert!(o.log.iter().all(|l| l.len() <= LOG_LINE_BYTES));
}

#[test]
fn a_failed_call_does_not_poison_the_next() {
    let p = hostile();
    let _ = p.on_tool_call(&call("hog"), &ctx());
    let _ = p.on_tool_call(&call("spin"), &ctx());
    assert_eq!(p.on_tool_call(&call("Read"), &ctx()).value, Verdict::Allow);
}

#[test]
fn shared_across_threads() {
    let p = std::sync::Arc::new(hostile());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let p = p.clone();
            std::thread::spawn(move || p.on_tool_call(&call(&format!("t{i}")), &ctx()).value)
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), Verdict::Allow);
    }
}

#[test]
fn rejects_what_is_not_a_policy_component() {
    for bytes in [&b""[..], b"not wasm", b"\0asm\x01\0\0\0"] {
        assert!(matches!(
            Policy::load("x", bytes),
            Err(PolicyError::Invalid(_))
        ));
    }
    let big = vec![0u8; wyrd_policy::MAX_COMPONENT_BYTES + 1];
    assert!(matches!(
        Policy::load("x", &big),
        Err(PolicyError::TooLarge)
    ));
    assert!(matches!(
        Policy::load_file(std::path::Path::new("/nonexistent/p.wasm")),
        Err(PolicyError::Read { .. })
    ));
}

#[test]
fn conformance_cases_pass_and_fail() {
    let cases = wyrd_policy::conformance::parse(
        r#"
[ctx]
repo_root = "/work/shop"

[kernel_rules]
fs_read_deny = ["~/.ssh"]
egress_allow_ports = [443]

[[case]]
name = "allowed"
tool_call = { tool = "Read" }
expect = "allow"

[[case]]
name = "denied with reason"
tool_call = { tool = "deny" }
expect = "deny"
reason_contains = "/work/shop"

[[case]]
name = "wrong expectation"
effect = { kind = "net.connect", host = "203.0.113.9", port = 443 }
expect = "allow"
"#,
    )
    .unwrap();
    let r = wyrd_policy::conformance::run(&hostile(), &cases);
    let passed: Vec<bool> = r.iter().map(|c| c.passed).collect();
    assert_eq!(passed, vec![true, true, true, false]);
    assert_eq!(r[3].got, "ask: 203.0.113.9:443");

    let missing = wyrd_policy::conformance::parse(
        "[ctx]\nrepo_root = \"/r\"\n[kernel_rules]\nexec_deny = [\"wget\"]\n",
    )
    .unwrap();
    let r = wyrd_policy::conformance::run(&hostile(), &missing);
    assert!(!r[0].passed);
    assert_eq!(r[0].got, "exec_deny lacks wget");

    for bad in [
        "[ctx]\nrepo_root = \"/r\"\n[[case]]\nname = \"x\"\nexpect = \"allow\"\n",
        "[ctx]\nrepo_root = \"/r\"\nbogus = 1\n",
        "[ctx]\nrepo_root = \"/r\"\n[[case]]\nname = \"x\"\ntool_call = { tool = \"a\" }\nexpect = \"maybe\"\n",
    ] {
        assert!(wyrd_policy::conformance::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn backtest_finds_what_the_policy_would_stop_deterministically() {
    use wyrd_schema::{
        Event, EventBody, FilePath, IntentSource, NetConnect, NetVerdict, ProcExec, ToolCallStarted,
    };
    let ev = |seq: u64, body: EventBody| Event {
        seq,
        ts_mono_ns: seq * 1000,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body,
    };
    let call = |tool: &str, source| {
        EventBody::ToolCallStarted(ToolCallStarted {
            call_id: format!("c-{tool}"),
            source,
            tool: tool.into(),
            args: serde_json::json!({"command": "x"}),
            turn: Some(1),
        })
    };
    let connect = |addr: &str| {
        EventBody::NetConnect(NetConnect {
            family: 4,
            addr: addr.into(),
            port: 443,
            dns_name: None,
            verdict: NetVerdict::Allowed,
        })
    };
    let events = vec![
        ev(1, call("Read", IntentSource::Hook)),
        ev(2, call("deny", IntentSource::Hook)),
        ev(3, call("ask", IntentSource::Mcp)),
        ev(4, connect("203.0.113.9")),
        ev(5, connect("10.1.2.3")),
        ev(
            6,
            EventBody::ProcExec(ProcExec {
                filename: "/usr/bin/curl".into(),
                argv: vec!["curl".into()],
                cwd: None,
                truncated: false,
            }),
        ),
        ev(
            7,
            EventBody::FileReadSensitive(FilePath {
                path: "/home/u/.ssh/id_ed25519".into(),
                in_cell: false,
                truncated: false,
            }),
        ),
    ];
    let p = hostile();
    let ws = std::path::Path::new("/work/shop");
    let home = Some(std::path::Path::new("/home/u"));
    let r = wyrd_policy::backtest::backtest(&p, &events, ws, home);
    let got: Vec<(u64, &str, &str)> = r
        .findings
        .iter()
        .map(|f| (f.seq, f.stage.as_str(), f.decision.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (2, "hook", "deny"),
            (3, "gateway", "ask"),
            (4, "kernel", "deny"),
            (5, "effect", "ask"),
            (6, "kernel", "deny"),
            (7, "kernel", "deny"),
        ]
    );
    assert_eq!(r.events, 7);
    assert_eq!(r.policy_id, "hostile");
    assert!(r.findings[2].reason.contains("203.0.113.9"));
    // Same policy, same events: the same report, from a fresh load too.
    assert_eq!(
        wyrd_policy::backtest::backtest(&hostile(), &events, ws, home),
        r
    );
}
