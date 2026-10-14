//! Playing a script.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::mcp_client::Client;
use crate::script::{self, Step, expand, expand_json};

/// Adapter whose hook format the fake agent speaks.
const ADAPTER: &str = "claude-code";

struct Agent {
    wyrd: Option<String>,
    cwd: PathBuf,
    session: String,
    transcript: PathBuf,
    clients: BTreeMap<String, Client>,
    servers: BTreeMap<String, Vec<String>>,
}

/// Result of executing one tool call.
struct Outcome {
    response: Value,
    error: Option<String>,
    shown: String,
}

fn ok(response: Value, shown: impl Into<String>) -> Outcome {
    Outcome {
        response,
        error: None,
        shown: shown.into(),
    }
}

fn failed(error: impl Into<String>) -> Outcome {
    let error = error.into();
    Outcome {
        response: Value::Null,
        shown: error.clone(),
        error: Some(error),
    }
}

fn say(line: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

impl Agent {
    fn base(&self, event: &str) -> Value {
        json!({
            "session_id": self.session,
            "transcript_path": self.transcript,
            "cwd": self.cwd,
            "hook_event_name": event,
        })
    }

    /// Send a hook payload through `wyrd hook`; returns the denial reason
    /// for a pre-execution check.
    fn hook(&self, payload: &Value) -> anyhow::Result<Option<String>> {
        let Some(wyrd) = &self.wyrd else {
            return Ok(None);
        };
        let mut child = Command::new(wyrd)
            .args(["hook", ADAPTER])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("running wyrd hook")?;
        child
            .stdin
            .take()
            .context("no stdin")?
            .write_all(payload.to_string().as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            say(&format!("  (hook exited with {:?})", out.status.code()));
        }
        let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
            return Ok(None);
        };
        let hso = &v["hookSpecificOutput"];
        Ok(match hso["permissionDecision"].as_str() {
            Some("deny" | "ask") => Some(
                hso["permissionDecisionReason"]
                    .as_str()
                    .unwrap_or("denied")
                    .to_string(),
            ),
            _ => None,
        })
    }

    fn record(&self, line: &Value) -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    fn client(&mut self, server: &str) -> anyhow::Result<&mut Client> {
        if !self.clients.contains_key(server) {
            let command = self.servers.get(server).cloned().unwrap_or_default();
            let c = Client::start(server, &command, self.wyrd.as_deref())?;
            self.clients.insert(server.to_string(), c);
        }
        self.clients.get_mut(server).context("MCP client")
    }

    fn path(&self, input: &Value) -> PathBuf {
        self.cwd
            .join(input["file_path"].as_str().unwrap_or_default())
    }

    fn execute(&mut self, step: &Step, input: &Value) -> anyhow::Result<Outcome> {
        if let Some(server) = &step.mcp {
            return Ok(match self.client(server)?.call(&step.tool, input)? {
                Ok(result) => {
                    let text = result["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if result["isError"].as_bool() == Some(true) {
                        failed(text)
                    } else {
                        ok(result, text)
                    }
                }
                Err(message) => failed(message),
            });
        }
        Ok(match step.tool.as_str() {
            "Bash" => {
                let cmd = input["command"].as_str().unwrap_or_default();
                let out = Command::new("sh")
                    .args(["-c", cmd])
                    .current_dir(&self.cwd)
                    .output()?;
                let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
                match out.status.code() {
                    Some(0) => ok(
                        json!({"stdout": stdout, "stderr": stderr, "interrupted": false, "isImage": false}),
                        stdout,
                    ),
                    code => failed(
                        format!("Exit code {}\n{stderr}", code.unwrap_or(-1))
                            .trim_end()
                            .to_string(),
                    ),
                }
            }
            "Write" => {
                let p = self.path(input);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let existed = p.exists();
                match std::fs::write(&p, input["content"].as_str().unwrap_or_default()) {
                    Ok(()) => ok(
                        json!({"type": if existed { "update" } else { "create" }, "filePath": p}),
                        format!("wrote {}", p.display()),
                    ),
                    Err(e) => failed(e.to_string()),
                }
            }
            "Edit" => {
                let p = self.path(input);
                let old = input["old_string"].as_str().unwrap_or_default();
                let new = input["new_string"].as_str().unwrap_or_default();
                match std::fs::read_to_string(&p) {
                    Ok(text) if !old.is_empty() && text.contains(old) => {
                        std::fs::write(&p, text.replacen(old, new, 1))?;
                        ok(
                            json!({"filePath": p, "oldString": old, "newString": new}),
                            format!("edited {}", p.display()),
                        )
                    }
                    Ok(_) => failed("String to replace not found in file."),
                    Err(e) => failed(e.to_string()),
                }
            }
            "Read" => match std::fs::read_to_string(self.path(input)) {
                Ok(text) => ok(
                    json!({"type": "text", "file": {"content": text}}),
                    format!("{} bytes", text.len()),
                ),
                Err(e) => failed(e.to_string()),
            },
            _ => ok(json!({}), "ok"),
        })
    }

    fn step(&mut self, id: &str, step: &Step) -> anyhow::Result<()> {
        if step.list {
            let server = step.mcp.clone().unwrap_or_default();
            let n = self.client(&server)?.list()?["tools"]
                .as_array()
                .map_or(0, Vec::len);
            say(&format!("• tools/list({server}) → {n} tools"));
            return Ok(());
        }
        let tool = match &step.mcp {
            Some(server) => format!("mcp__{server}__{}", step.tool),
            None => step.tool.clone(),
        };
        let input = expand_json(&step.input);
        let mut pre = self.base("PreToolUse");
        pre["tool_name"] = json!(tool);
        pre["tool_input"] = input.clone();
        pre["tool_use_id"] = json!(id);
        if let Some(reason) = self.hook(&pre)? {
            say(&format!("• {tool} denied: {reason}"));
            return Ok(());
        }
        let started = Instant::now();
        let outcome = self.execute(step, &input)?;
        let duration = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut post = pre.clone();
        match &outcome.error {
            None => {
                post["hook_event_name"] = json!("PostToolUse");
                post["tool_response"] = outcome.response.clone();
            }
            Some(e) => {
                post["hook_event_name"] = json!("PostToolUseFailure");
                post["error"] = json!(e);
                post["is_interrupt"] = json!(false);
            }
        }
        post["duration_ms"] = json!(duration);
        self.hook(&post)?;
        let mark = if outcome.error.is_some() {
            "✗"
        } else {
            "✓"
        };
        say(&format!(
            "• {tool} {mark} {}",
            outcome.shown.lines().next().unwrap_or("")
        ));
        Ok(())
    }
}

pub fn run(path: &Path) -> anyhow::Result<u8> {
    let script = script::load(path)?;
    let cwd = std::env::current_dir()?;
    let mut agent = Agent {
        wyrd: std::env::var("WYRD_BIN").ok().filter(|s| !s.is_empty()),
        session: format!("fakeagent-{}", script.name),
        transcript: std::env::temp_dir()
            .join(format!("wyrd-fakeagent-{}.jsonl", std::process::id())),
        cwd,
        clients: BTreeMap::new(),
        servers: script
            .mcp
            .iter()
            .map(|(k, v)| (k.clone(), v.command.clone()))
            .collect(),
    };
    say(&format!("# {} — {}", script.name, script.description));
    agent.hook(&agent.base("SessionStart"))?;
    for (t, turn) in script.turns.iter().enumerate() {
        let prompt = expand(&turn.prompt);
        say(&format!("> {prompt}"));
        agent.record(&json!({"type": "user", "message": {"role": "user", "content": prompt}}))?;
        let mut submit = agent.base("UserPromptSubmit");
        submit["prompt"] = json!(prompt);
        agent.hook(&submit)?;
        for (s, step) in turn.steps.iter().enumerate() {
            agent.step(&format!("fa-{}-{}", t + 1, s + 1), step)?;
        }
        let replies: Vec<String> = turn.reply.iter().map(|r| expand(r)).collect();
        for r in &replies {
            say(r);
            agent.record(&json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": r}]}}))?;
        }
        let mut stop = agent.base("Stop");
        stop["stop_hook_active"] = json!(false);
        stop["last_assistant_message"] = json!(replies.last().cloned().unwrap_or_default());
        agent.hook(&stop)?;
    }
    for (_, c) in std::mem::take(&mut agent.clients) {
        c.close()?;
    }
    let mut end = agent.base("SessionEnd");
    end["reason"] = json!("other");
    agent.hook(&end)?;
    let _ = std::fs::remove_file(&agent.transcript);
    Ok(script.exit_code)
}
