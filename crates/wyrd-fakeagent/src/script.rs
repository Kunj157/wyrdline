//! The `.wyrdscript.toml` format.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;

/// A scripted session.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// MCP servers by name; `wyrd-fakeagent` as the program means this binary.
    #[serde(default)]
    pub mcp: BTreeMap<String, McpServer>,
    #[serde(default, rename = "turn")]
    pub turns: Vec<Turn>,
    /// Exit code of the agent after the last turn.
    #[serde(default)]
    pub exit_code: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub prompt: String,
    /// Assistant texts of the turn; the last one is the final message.
    #[serde(default)]
    pub reply: Vec<String>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

/// One tool call. Built-in tools use the same input shape as the real
/// agent: `Bash {command}`, `Write {file_path, content}`,
/// `Edit {file_path, old_string, new_string}`, `Read {file_path}`; any
/// other tool succeeds without doing anything. With `mcp`, `tool` is
/// called on that server; with `list = true`, the server's tools are listed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub mcp: Option<String>,
    #[serde(default)]
    pub list: bool,
    #[serde(default = "empty_input")]
    pub input: serde_json::Value,
}

fn empty_input() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Replace `${NAME}` with the environment variable `NAME` (empty if unset).
pub fn expand(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            out.push_str(&std::env::var(&after[..end]).unwrap_or_default());
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Expand every string inside a JSON value.
pub fn expand_json(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(expand(s)),
        Value::Array(a) => Value::Array(a.iter().map(expand_json).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), expand_json(x))).collect())
        }
        other => other.clone(),
    }
}

pub fn load(path: &Path) -> anyhow::Result<Script> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let script: Script =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (t, turn) in script.turns.iter().enumerate() {
        for (s, step) in turn.steps.iter().enumerate() {
            if let Some(server) = &step.mcp
                && !script.mcp.contains_key(server)
            {
                anyhow::bail!("turn {} step {}: unknown MCP server {server}", t + 1, s + 1);
            }
            if step.tool.is_empty() && !step.list {
                anyhow::bail!("turn {} step {}: no tool", t + 1, s + 1);
            }
        }
    }
    Ok(script)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn expands_environment_references() {
        // PATH is always set for tests; an unset variable becomes empty.
        let path = std::env::var("PATH").unwrap();
        assert_eq!(expand("a ${PATH} b"), format!("a {path} b"));
        assert_eq!(expand("x${WYRD_SURELY_UNSET_VAR}y"), "xy");
        assert_eq!(expand("open ${"), "open ${");
        let v = expand_json(&serde_json::json!({"k": ["${WYRD_SURELY_UNSET_VAR}!"]}));
        assert_eq!(v, serde_json::json!({"k": ["!"]}));
    }

    #[test]
    fn rejects_unknown_servers_fields_and_empty_steps() {
        let dir = tempfile_dir();
        for (bad, why) in [
            (
                "name = \"x\"\n[[turn]]\nprompt = \"p\"\n[[turn.step]]\nmcp = \"nope\"\ntool = \"t\"\n",
                "unknown MCP server",
            ),
            (
                "name = \"x\"\n[[turn]]\nprompt = \"p\"\n[[turn.step]]\ninput = {}\n",
                "no tool",
            ),
            ("name = \"x\"\nsurprise = 1\n", "unknown field"),
        ] {
            let p = dir.join("s.toml");
            std::fs::write(&p, bad).unwrap();
            let err = format!("{:#}", load(&p).unwrap_err());
            assert!(err.contains(why), "{err}");
        }
    }

    #[test]
    fn every_demo_script_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/scenarios");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.to_string_lossy().ends_with(".wyrdscript.toml") {
                load(&p).unwrap();
                n += 1;
            }
        }
        assert!(n > 0);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wyrd-fakeagent-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
