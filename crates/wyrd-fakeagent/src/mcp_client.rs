//! Minimal MCP client over stdio, routed through `wyrd mcp-proxy` when
//! running under `wyrd run`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Start `command` (through the gateway when `wyrd` is given) and
    /// perform the MCP handshake, including the first `tools/list`.
    pub fn start(name: &str, command: &[String], wyrd: Option<&str>) -> anyhow::Result<Client> {
        let mut argv: Vec<String> = Vec::new();
        if let Some(w) = wyrd {
            argv.extend([
                w.to_string(),
                "mcp-proxy".into(),
                "--server".into(),
                name.to_string(),
                "--".into(),
            ]);
        }
        let me = std::env::current_exe().context("locating wyrd-fakeagent")?;
        for (i, a) in command.iter().enumerate() {
            // The toy server is this binary.
            if i == 0 && a == "wyrd-fakeagent" {
                argv.push(me.display().to_string());
            } else {
                argv.push(a.clone());
            }
        }
        let (prog, prog_args) = argv.split_first().context("empty MCP server command")?;
        let mut child = Command::new(prog)
            .args(prog_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting MCP server {name}"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
        let mut c = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        c.request(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "wyrd-fakeagent", "version": env!("CARGO_PKG_VERSION")}}),
        )?
        .map_err(anyhow::Error::msg)?;
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        c.list()?;
        Ok(c)
    }

    fn send(&mut self, msg: &Value) -> anyhow::Result<()> {
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// One request; `Ok(Err(message))` for a JSON-RPC error response.
    pub fn request(
        &mut self,
        method: &str,
        params: &Value,
    ) -> anyhow::Result<Result<Value, String>> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                bail!("MCP server closed the connection");
            }
            let v: Value = serde_json::from_str(&line).context("MCP server sent invalid JSON")?;
            if v.get("id") != Some(&json!(id)) {
                continue;
            }
            return Ok(match v.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            });
        }
    }

    pub fn list(&mut self) -> anyhow::Result<Value> {
        self.request("tools/list", &json!({}))?
            .map_err(anyhow::Error::msg)
    }

    pub fn call(&mut self, tool: &str, arguments: &Value) -> anyhow::Result<Result<Value, String>> {
        self.request("tools/call", &json!({"name": tool, "arguments": arguments}))
    }

    /// Close stdin and wait for the server (and gateway) to exit.
    pub fn close(self) -> anyhow::Result<()> {
        let Client {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        child.wait()?;
        Ok(())
    }
}
