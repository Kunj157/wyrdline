//! Policy backtest (plan §17.3): replay a recorded session against a
//! policy and list what it would have stopped. Pure apart from the policy
//! calls, which are deterministic (ADR-007), so the same policy and the
//! same events always give the same findings.
//!
//! Kernel rules are simulated on the recorded effects: connects against
//! the egress prefixes, names, and ports; sensitive reads against the read
//! deny-list; writes that escaped the Cell against the write allow-list;
//! execs against the exec deny list (by name: a recording has no
//! binaries to hash). Host names match a connect's recorded DNS name only;
//! nothing is resolved.

use std::path::Path;

use serde::{Deserialize, Serialize};
use wyrd_schema::{Event, EventBody, IntentSource};

use crate::effects::effect_of;
use crate::kernel::{Cidr, Compiled, compile};
use crate::{Policy, SessionCtx, ToolCall, Verdict};

/// One action the policy would have stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The event the decision is about.
    pub seq: u64,
    /// `hook`, `gateway`, `kernel`, or `effect`.
    pub stage: String,
    /// `deny` or `ask`.
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub policy_id: String,
    pub policy_version: String,
    pub events: u64,
    pub findings: Vec<Finding>,
    /// Kernel rules the policy could not compile, or why it gave none.
    pub notes: Vec<String>,
}

fn contains(c: &Cidr, addr: std::net::IpAddr) -> bool {
    let bits = |a: std::net::IpAddr| -> (u128, u32) {
        match a {
            std::net::IpAddr::V4(v) => (u128::from(u32::from(v)), 32),
            std::net::IpAddr::V6(v) => (u128::from(v), 128),
        }
    };
    let addr = match addr {
        std::net::IpAddr::V6(v) => v.to_ipv4_mapped().map_or(addr, std::net::IpAddr::V4),
        v4 @ std::net::IpAddr::V4(_) => v4,
    };
    let (a, la) = bits(addr);
    let (n, ln) = bits(c.addr);
    if la != ln {
        return false;
    }
    let p = u32::from(c.prefix);
    if p == 0 {
        return true;
    }
    let shift = la - p;
    (a >> shift) == (n >> shift)
}

fn under(path: &str, dirs: &[std::path::PathBuf]) -> bool {
    dirs.iter().any(|d| Path::new(path).starts_with(d))
}

/// What the kernel rules would have refused for one effect.
fn kernel(k: &Compiled, body: &EventBody, workspace: &Path) -> Option<String> {
    match body {
        EventBody::NetConnect(c) => {
            if let Some(cidrs) = &k.egress_cidrs {
                let ip: Option<std::net::IpAddr> = c.addr.parse().ok();
                let by_prefix = ip.is_some_and(|ip| cidrs.iter().any(|n| contains(n, ip)));
                let by_name = c
                    .dns_name
                    .as_ref()
                    .is_some_and(|n| k.egress_hosts.iter().any(|h| h == n));
                if !by_prefix && !by_name {
                    return Some(format!(
                        "connect to {}{} is not on the egress allow-list",
                        c.addr,
                        c.dns_name
                            .as_ref()
                            .map(|n| format!(" ({n})"))
                            .unwrap_or_default()
                    ));
                }
            }
            match &k.tcp_connect_ports {
                Some(ports) if !ports.contains(&c.port) => {
                    Some(format!("connect to port {} is not allowed", c.port))
                }
                _ => None,
            }
        }
        EventBody::FileReadSensitive(f) if under(&f.path, &k.read_deny) => {
            Some(format!("reading {} is denied", f.path))
        }
        EventBody::FileOpenWrite(w)
            if !w.file.in_cell
                && !Path::new(&w.file.path).starts_with(workspace)
                && !under(&w.file.path, &k.write_allow)
                && !["/tmp", "/var/tmp", "/dev", "/proc"]
                    .iter()
                    .any(|d| Path::new(&w.file.path).starts_with(d)) =>
        {
            Some(format!(
                "writing {} is outside the write allow-list",
                w.file.path
            ))
        }
        EventBody::ProcExec(x) => {
            let name = x.filename.rsplit('/').next().unwrap_or(&x.filename);
            k.exec_deny
                .iter()
                .any(|d| d == name)
                .then(|| format!("executing {name} is denied"))
        }
        _ => None,
    }
}

fn finding(seq: u64, stage: &str, v: Verdict) -> Option<Finding> {
    let (decision, reason) = match v {
        Verdict::Allow => return None,
        Verdict::Deny(r) => ("deny", r),
        Verdict::Ask(r) => ("ask", r),
    };
    Some(Finding {
        seq,
        stage: stage.into(),
        decision: decision.into(),
        reason,
    })
}

/// Replay `events` (one session, in order) against `policy`.
pub fn backtest(
    policy: &Policy,
    events: &[Event],
    workspace: &Path,
    home: Option<&Path>,
) -> Report {
    let ctx = |prior: u32| SessionCtx {
        repo_root: workspace.display().to_string(),
        scope_globs: Vec::new(),
        prior_denials: prior,
    };
    let mut notes = Vec::new();
    let rules = policy.kernel_rules(&ctx(0));
    let compiled = if let Some(r) = rules.value {
        let c = compile(&r, home, workspace);
        notes.extend(c.not_expressible.iter().cloned());
        Some(c)
    } else {
        notes.push(format!(
            "kernel-rules failed ({}); such a session would not start",
            rules.error.unwrap_or_default()
        ));
        None
    };
    let mut findings = Vec::new();
    let mut denials = 0u32;
    for ev in events {
        let found = match &ev.body {
            EventBody::ToolCallStarted(t) => {
                let (stage, adapter) = match t.source {
                    IntentSource::Mcp => ("gateway", "mcp"),
                    IntentSource::Hook | IntentSource::Otlp => ("hook", "hook"),
                };
                let call = ToolCall {
                    tool: t.tool.clone(),
                    args_json: t.args.to_string(),
                    adapter: adapter.into(),
                    cwd: workspace.display().to_string(),
                };
                finding(
                    ev.seq,
                    stage,
                    policy.on_tool_call(&call, &ctx(denials)).value,
                )
            }
            body => compiled
                .as_ref()
                .and_then(|k| kernel(k, body, workspace))
                .map(|reason| Finding {
                    seq: ev.seq,
                    stage: "kernel".into(),
                    decision: "deny".into(),
                    reason,
                })
                .or_else(|| {
                    let e = effect_of(body)?;
                    finding(ev.seq, "effect", policy.on_effect(&e, &ctx(denials)).value)
                }),
        };
        if let Some(f) = found {
            if f.decision == "deny" {
                denials += 1;
            }
            findings.push(f);
        }
    }
    Report {
        policy_id: policy.id.clone(),
        policy_version: policy.version.clone(),
        events: events.len() as u64,
        findings,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_match_v4_v6_and_mapped() {
        let c = crate::kernel::parse_cidr("10.0.0.0/8").unwrap();
        assert!(contains(&c, "10.2.3.4".parse().unwrap()));
        assert!(contains(&c, "::ffff:10.2.3.4".parse().unwrap()));
        assert!(!contains(&c, "11.0.0.1".parse().unwrap()));
        let v6 = crate::kernel::parse_cidr("2001:db8::/32").unwrap();
        assert!(contains(&v6, "2001:db8::1".parse().unwrap()));
        assert!(!contains(&v6, "10.2.3.4".parse().unwrap()));
        let any = crate::kernel::parse_cidr("0.0.0.0/0").unwrap();
        assert!(contains(&any, "8.8.8.8".parse().unwrap()));
    }
}
