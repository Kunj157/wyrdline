//! The Wasm policy of `wyrd run --policy`, as the intent socket's
//! pre-execution policy.

use serde_json::Value;
use wyrd_adapters::ToolPolicy;
use wyrd_policy::{Policy, SessionCtx, ToolCall, Verdict};
use wyrd_schema::Decision;

#[derive(Debug)]
pub struct WasmToolPolicy {
    policy: std::sync::Arc<Policy>,
    ctx: SessionCtx,
}

impl WasmToolPolicy {
    pub fn new(policy: std::sync::Arc<Policy>, repo_root: &std::path::Path) -> Self {
        WasmToolPolicy {
            policy,
            ctx: SessionCtx {
                repo_root: repo_root.display().to_string(),
                scope_globs: Vec::new(),
                prior_denials: 0,
            },
        }
    }
}

pub fn decision(v: Verdict) -> (Decision, String) {
    match v {
        Verdict::Allow => (Decision::Allow, "the policy allows this call".into()),
        Verdict::Deny(r) => (Decision::Deny, r),
        Verdict::Ask(r) => (Decision::Ask, r),
    }
}

impl ToolPolicy for WasmToolPolicy {
    fn id(&self) -> &str {
        &self.policy.id
    }

    fn version(&self) -> &str {
        &self.policy.version
    }

    fn on_tool_call(
        &self,
        tool: &str,
        args: &Value,
        adapter: &str,
        prior_denials: u32,
    ) -> (Decision, String) {
        let ctx = SessionCtx {
            prior_denials,
            ..self.ctx.clone()
        };
        let out = self.policy.on_tool_call(
            &ToolCall {
                tool: tool.into(),
                args_json: args.to_string(),
                adapter: adapter.into(),
                cwd: self.ctx.repo_root.clone(),
            },
            &ctx,
        );
        if let Some(e) = &out.error {
            tracing::warn!(policy = %self.policy.id, error = %e, "policy failed; denying");
        }
        decision(out.value)
    }
}
