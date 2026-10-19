//! Conformance cases for `wyrd policy test`: a TOML file of tool calls and
//! effects with the decision a policy must return for each.
//!
//! ```toml
//! [ctx]
//! repo_root = "/work/shop"
//!
//! [kernel_rules]          # optional; each list must be contained in the policy's
//! fs_read_deny = ["~/.ssh"]
//!
//! [[case]]
//! name = "curl to an unknown host asks"
//! tool_call = { tool = "Bash", args_json = '{"command":"curl https://evil.example"}' }
//! expect = "ask"
//! reason_contains = "evil.example"
//! ```

use serde::Deserialize;

use crate::{Effect, KernelRules, Policy, SessionCtx, ToolCall, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    #[serde(default)]
    pub tool_call: Option<ToolCall>,
    #[serde(default)]
    pub effect: Option<Effect>,
    pub expect: Expect,
    #[serde(default)]
    pub reason_contains: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cases {
    pub ctx: SessionCtx,
    #[serde(default)]
    pub kernel_rules: Option<KernelRules>,
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

/// Outcome of one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub name: String,
    pub passed: bool,
    /// What the policy returned, in words.
    pub got: String,
}

pub fn parse(text: &str) -> Result<Cases, String> {
    let cases: Cases = toml::from_str(text).map_err(|e| e.to_string())?;
    for c in &cases.cases {
        if c.tool_call.is_some() == c.effect.is_some() {
            return Err(format!(
                "case {:?}: give exactly one of tool_call and effect",
                c.name
            ));
        }
    }
    Ok(cases)
}

fn describe(v: &Verdict) -> String {
    match v {
        Verdict::Allow => "allow".into(),
        Verdict::Deny(r) => format!("deny: {r}"),
        Verdict::Ask(r) => format!("ask: {r}"),
    }
}

fn contained(want: &[String], have: &[String]) -> Vec<String> {
    want.iter().filter(|w| !have.contains(w)).cloned().collect()
}

fn check_rules(policy: &Policy, cases: &Cases, want: &KernelRules) -> CaseResult {
    let got = policy.kernel_rules(&cases.ctx);
    let Some(have) = got.value else {
        return CaseResult {
            name: "kernel rules".into(),
            passed: false,
            got: format!("error: {}", got.error.unwrap_or_default()),
        };
    };
    let ports = |v: &[u16]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    let missing: Vec<String> = [
        (
            "fs_read_deny",
            contained(&want.fs_read_deny, &have.fs_read_deny),
        ),
        (
            "fs_write_allow",
            contained(&want.fs_write_allow, &have.fs_write_allow),
        ),
        ("exec_deny", contained(&want.exec_deny, &have.exec_deny)),
        (
            "egress_allow_cidrs",
            contained(&want.egress_allow_cidrs, &have.egress_allow_cidrs),
        ),
        (
            "egress_allow_hosts",
            contained(&want.egress_allow_hosts, &have.egress_allow_hosts),
        ),
        (
            "egress_allow_ports",
            contained(
                &ports(&want.egress_allow_ports),
                &ports(&have.egress_allow_ports),
            ),
        ),
    ]
    .into_iter()
    .filter(|(_, m)| !m.is_empty())
    .map(|(k, m)| format!("{k} lacks {}", m.join(", ")))
    .collect();
    CaseResult {
        name: "kernel rules".into(),
        passed: missing.is_empty(),
        got: if missing.is_empty() {
            "contains the expected rules".into()
        } else {
            missing.join("; ")
        },
    }
}

/// Run every case (and the kernel-rules check, when given) in file order.
pub fn run(policy: &Policy, cases: &Cases) -> Vec<CaseResult> {
    let mut out = Vec::new();
    if let Some(want) = &cases.kernel_rules {
        out.push(check_rules(policy, cases, want));
    }
    for c in &cases.cases {
        let v = match (&c.tool_call, &c.effect) {
            (Some(t), _) => policy.on_tool_call(t, &cases.ctx).value,
            (None, Some(e)) => policy.on_effect(e, &cases.ctx).value,
            (None, None) => Verdict::Deny("case has neither tool_call nor effect".into()),
        };
        let kind_ok = matches!(
            (c.expect, &v),
            (Expect::Allow, Verdict::Allow)
                | (Expect::Deny, Verdict::Deny(_))
                | (Expect::Ask, Verdict::Ask(_))
        );
        let reason_ok = c
            .reason_contains
            .as_deref()
            .is_none_or(|s| v.reason().contains(s));
        out.push(CaseResult {
            name: c.name.clone(),
            passed: kind_ok && reason_ok,
            got: describe(&v),
        });
    }
    out
}
