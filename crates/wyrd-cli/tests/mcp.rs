#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd mcp` driven over stdio with JSON-RPC, as an MCP client would:
//! the handshake, tool listing, every tool, protocol errors, tool errors,
//! and that a rewind request rewinds nothing.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

struct Env {
    tmp: tempfile::TempDir,
    data: DataDir,
    id: String,
}

/// A Cell-mode session with checkpoints c1 and c2 and a synthetic run of
/// tool calls and kernel effects (so there is something to explain).
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
            name: None,
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
    wyrd_ledger::Ledger::open(s.dir().ledger())
        .unwrap()
        .append(wyrd_correlator::synth::session(3).events)
        .unwrap();
    let id = s.record().id.clone();
    Env { tmp, data, id }
}

fn wyrd(e: &Env) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wyrd"));
    c.env("WYRD_DATA_DIR", e.data.root())
        .env("XDG_STATE_HOME", e.tmp.path().join("state"))
        .env_remove("WYRD_SESSION_ID")
        .env_remove("WYRD_INTENT_SOCKET")
        .env_remove("WYRD_CELL");
    c
}

/// A running `wyrd mcp` with a line reader that never blocks the test
/// for more than a few seconds.
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Server {
    fn start(mut cmd: Command) -> Server {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Server {
            child,
            stdin,
            lines,
        }
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("no answer from wyrd mcp");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    fn request(&mut self, id: u64, method: &str, params: &Value) -> Value {
        self.send_raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        let v = self.recv();
        assert_eq!(v["jsonrpc"], "2.0", "{v}");
        assert_eq!(v["id"], id, "{v}");
        v
    }

    fn call(&mut self, id: u64, tool: &str, args: &Value) -> Value {
        let v = self.request(id, "tools/call", &json!({"name": tool, "arguments": args}));
        assert!(v.get("error").is_none(), "{v}");
        v["result"].clone()
    }

    fn initialize(&mut self) -> Value {
        let v = self.request(
            0,
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}}),
        );
        self.send_raw(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        );
        v
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn mcp(e: &Env) -> Server {
    let mut c = wyrd(e);
    c.args(["mcp", "--session", &e.id]);
    Server::start(c)
}

/// The JSON a tool returned, from its structured content.
fn structured(result: &Value) -> &Value {
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["content"][0]["type"], "text");
    &result["structuredContent"]
}

fn tool_error(result: &Value) -> String {
    assert_eq!(result["isError"], true, "{result}");
    result["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn handshake_and_tool_listing() {
    let e = env();
    let mut s = mcp(&e);
    let init = s.initialize();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "wyrdline");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let list = s.request(1, "tools/list", &json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "list_checkpoints",
            "show_changes_since",
            "explain_effect",
            "list_divergences",
            "request_rewind"
        ]
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert!(t["description"].as_str().unwrap().len() > 20);
        let read_only = t["name"] != "request_rewind";
        assert_eq!(t["annotations"]["readOnlyHint"], read_only, "{t}");
    }
    let rewind = &tools[4]["inputSchema"];
    assert_eq!(rewind["required"], json!(["checkpoint_id", "reason"]));

    // An older client gets its own version back; an unknown one ours.
    let old = s.request(
        2,
        "initialize",
        &json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
    );
    assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    let future = s.request(3, "initialize", &json!({"protocolVersion": "2099-01-01"}));
    assert_eq!(future["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(s.request(4, "ping", &json!({}))["result"], json!({}));
}

#[test]
fn read_only_tools_answer_from_the_bound_session() {
    let e = env();
    let mut s = mcp(&e);
    s.initialize();
    let cps = s.call(1, "list_checkpoints", &json!({}));
    let cps = structured(&cps);
    assert_eq!(cps["current_branch"], "a");
    let ids: Vec<&str> = cps["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c1", "c2"]);

    let changes = s.call(2, "show_changes_since", &json!({"checkpoint_id": "c1"}));
    let changes = structured(&changes);
    assert_eq!(changes["from"], "c1");
    assert_eq!(changes["to"], "c2");
    assert_eq!(changes["changes"][0]["path"], "b.txt");
    assert_eq!(changes["changes"][0]["kind"], "added");

    let graph = wyrd(&e).args(["graph", &e.id, "--json"]).output().unwrap();
    let graph: Value = serde_json::from_slice(&graph.stdout).unwrap();
    let seq = graph["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["call_id"].is_string())
        .unwrap()["seq"]
        .as_u64()
        .unwrap();
    let ex = s.call(3, "explain_effect", &json!({"seq": seq}));
    let ex = structured(&ex);
    assert_eq!(ex["effect"]["seq"], seq);
    assert!(ex["call"]["call_id"].is_string(), "{ex}");
    assert_ne!(ex["process_chain"].as_array().unwrap().len(), 0);

    let dv = s.call(4, "list_divergences", &json!({}));
    assert!(structured(&dv)["divergences"].is_array());
}

#[test]
fn unknown_checkpoints_and_seqs_are_tool_errors() {
    let e = env();
    let mut s = mcp(&e);
    s.initialize();
    let r = s.call(1, "show_changes_since", &json!({"checkpoint_id": "c9"}));
    assert!(tool_error(&r).contains("c9"));
    let r = s.call(2, "explain_effect", &json!({"seq": 999_999}));
    assert!(tool_error(&r).contains("999999"));
    let r = s.call(
        3,
        "request_rewind",
        &json!({"checkpoint_id": "c9", "reason": "x"}),
    );
    assert!(tool_error(&r).contains("c9"));
    // The server is still serving.
    let cps = s.call(4, "list_checkpoints", &json!({}));
    structured(&cps);
}

#[test]
fn protocol_errors_are_json_rpc_errors() {
    let e = env();
    let mut s = mcp(&e);
    s.initialize();
    let code = |v: &Value| v["error"]["code"].as_i64().unwrap();
    assert_eq!(code(&s.request(1, "resources/list", &json!({}))), -32601);
    let unknown = s.request(2, "tools/call", &json!({"name": "rewind", "arguments": {}}));
    assert_eq!(code(&unknown), -32602);
    let missing = s.request(
        3,
        "tools/call",
        &json!({"name": "explain_effect", "arguments": {}}),
    );
    assert_eq!(code(&missing), -32602);
    let wrong_type = s.request(
        4,
        "tools/call",
        &json!({"name": "explain_effect", "arguments": {"seq": "12"}}),
    );
    assert_eq!(code(&wrong_type), -32602);
    let no_name = s.request(5, "tools/call", &json!({}));
    assert_eq!(code(&no_name), -32602);

    s.send_raw("{not json");
    let v = s.recv();
    assert_eq!(code(&v), -32700);
    assert_eq!(v["id"], Value::Null);
    s.send_raw(r#"{"jsonrpc":"2.0","id":9}"#);
    let v = s.recv();
    assert_eq!(code(&v), -32600);
    assert_eq!(v["id"], 9);
    // A notification is never answered; the next request still is.
    s.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#);
    assert_eq!(s.request(10, "ping", &json!({}))["result"], json!({}));
}

#[test]
fn a_rewind_request_rewinds_nothing() {
    let e = env();
    let mut s = mcp(&e);
    s.initialize();
    let r = s.call(
        1,
        "request_rewind",
        &json!({"checkpoint_id": "c1", "reason": "my last edit broke the tests"}),
    );
    let out = structured(&r);
    assert_eq!(out["status"], "pending human confirmation");
    assert_eq!(out["request"]["checkpoint_id"], "c1");
    assert_eq!(out["request"]["status"], "pending");
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pending human confirmation"), "{text}");
    let rid = out["request"]["id"].as_u64().unwrap();

    let session = Session::open(&e.data, &e.id).unwrap();
    assert_eq!(session.record().branches.len(), 1);
    assert_eq!(session.record().current_branch, "a");
    let kinds: Vec<&str> = wyrd_ledger::Ledger::open_read_only(session.dir().ledger())
        .unwrap()
        .events_after(0, usize::MAX)
        .unwrap()
        .iter()
        .map(wyrd_schema::Event::kind)
        .collect();
    assert!(kinds.contains(&"meta.rewind.requested"));
    assert!(!kinds.contains(&"meta.rewind.confirmed"));
    assert!(!kinds.contains(&"state.checkpoint.rewound"));
    let reqs = session.rewind_requests().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].id, rid);
    assert_eq!(reqs[0].source, "mcp");
}

#[test]
fn secrets_in_a_reason_are_redacted() {
    let e = env();
    let mut s = mcp(&e);
    s.initialize();
    // Built at runtime: no secret-shaped literal in the tree.
    let key = format!("AKIA{}", "Q".repeat(16));
    let r = s.call(
        1,
        "request_rewind",
        &json!({"checkpoint_id": "c2", "reason": format!("leaked {key} by mistake")}),
    );
    structured(&r);
    let reqs = Session::open(&e.data, &e.id)
        .unwrap()
        .rewind_requests()
        .unwrap();
    assert!(!reqs[0].reason.contains(&key), "{}", reqs[0].reason);
}

#[test]
fn without_a_session_the_server_refuses_to_start() {
    let e = env();
    let out = wyrd(&e).arg("mcp").stdin(Stdio::null()).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--session"), "{err}");
    let out = wyrd(&e)
        .args(["mcp", "--session", "zzzz"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    // The session the Cell sets is used when no --session is given.
    let mut c = wyrd(&e);
    c.arg("mcp").env("WYRD_SESSION_ID", &e.id);
    let mut s = Server::start(c);
    s.initialize();
    structured(&s.call(1, "list_checkpoints", &json!({})));
}
