#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Mutex;

use serde_json::{Value, json};
use wyrd_adapters::{Direction, Response};
use wyrd_gateway::{FromClient, Gateway, Reporter};
use wyrd_schema::{CaptureMode, Decision};

#[derive(Debug, Default)]
struct Mock {
    seen: Mutex<Vec<(Direction, Value)>>,
    deny: bool,
    down: bool,
}

impl Reporter for Mock {
    fn report(&self, _: &str, d: Direction, m: &Value) -> Result<Response, String> {
        if self.down {
            return Err("connection refused".into());
        }
        self.seen.lock().unwrap().push((d, m.clone()));
        let is_call = m["method"] == "tools/call";
        Ok(if is_call {
            Response::Decision {
                decision: if self.deny {
                    Decision::Deny
                } else {
                    Decision::Allow
                },
                reason: self.deny.then(|| "tool echo is denied".to_string()),
            }
        } else {
            Response::Ok
        })
    }
}

fn gw(mock: Mock, mode: CaptureMode) -> Gateway<Mock> {
    Gateway {
        server: "toy".into(),
        mode,
        reporter: Some(mock),
    }
}

fn call(id: u32) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "echo", "arguments": {}}}).to_string()
}

#[test]
fn allowed_messages_are_forwarded_unchanged_and_reported() {
    let g = gw(Mock::default(), CaptureMode::Cell);
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}).to_string();
    assert_eq!(g.from_client(&init), FromClient::Forward(init.clone()));
    assert_eq!(g.from_client(&call(1)), FromClient::Forward(call(1)));
    g.from_server(r#"{"jsonrpc":"2.0","id":1,"result":{"content":[]}}"#);
    let seen = g.reporter.as_ref().unwrap().seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].0, Direction::ToClient);
}

#[test]
fn denied_call_gets_a_json_rpc_error_with_the_reason() {
    let g = gw(
        Mock {
            deny: true,
            ..Mock::default()
        },
        CaptureMode::Cell,
    );
    let FromClient::Reply(r) = g.from_client(&call(7)) else {
        panic!()
    };
    let v: Value = serde_json::from_str(&r).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["error"]["code"], wyrd_gateway::DENIED_CODE);
    assert_eq!(v["error"]["data"]["wyrdline_reason"], "tool echo is denied");
}

#[test]
fn batches_are_split_into_forwarded_and_denied_parts() {
    let g = gw(
        Mock {
            deny: true,
            ..Mock::default()
        },
        CaptureMode::Cell,
    );
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let batch = json!([list, serde_json::from_str::<Value>(&call(2)).unwrap()]).to_string();
    let FromClient::Both { forward, reply } = g.from_client(&batch) else {
        panic!()
    };
    let f: Value = serde_json::from_str(&forward).unwrap();
    let r: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(f.as_array().unwrap().len(), 1);
    assert_eq!(f[0]["method"], "tools/list");
    assert_eq!(r[0]["id"], 2);
}

#[test]
fn socket_down_fails_closed_in_cell_and_open_in_observe() {
    let g = gw(
        Mock {
            down: true,
            ..Mock::default()
        },
        CaptureMode::Cell,
    );
    assert!(matches!(g.from_client(&call(1)), FromClient::Reply(_)));
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string();
    assert_eq!(
        g.from_client(&list),
        FromClient::Forward(list.clone()),
        "only calls are blocked"
    );
    let g = gw(
        Mock {
            down: true,
            ..Mock::default()
        },
        CaptureMode::Observe,
    );
    assert_eq!(g.from_client(&call(1)), FromClient::Forward(call(1)));
}

#[test]
fn non_json_and_plain_relay_pass_through() {
    let g = gw(Mock::default(), CaptureMode::Cell);
    assert_eq!(
        g.from_client("not json"),
        FromClient::Forward("not json".into())
    );
    let plain: Gateway<Mock> = Gateway {
        server: "toy".into(),
        mode: CaptureMode::Cell,
        reporter: None,
    };
    assert_eq!(plain.from_client(&call(1)), FromClient::Forward(call(1)));
}
