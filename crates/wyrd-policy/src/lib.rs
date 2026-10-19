//! Wasm policy host (plan §17.3, ADR-007).
//!
//! A policy is a WebAssembly component implementing the `wyrdline:policy`
//! world (`policies/wit/policy.wit`). Every call gets a fresh instance with
//! a fuel budget and a memory cap. Every import other than `log` is linked
//! as a trap, so a policy has no filesystem, network, clock, or entropy, and
//! its decisions are a pure function of its inputs. Any failure (trap, fuel
//! exhausted, memory cap, malformed component) becomes a deny.

pub mod backtest;
pub mod conformance;
pub mod effects;
pub mod kernel;

use std::path::Path;

use serde::{Deserialize, Serialize};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../policies/wit",
        world: "policy",
    });
}

use bindings::wyrdline::policy::types as wit;

/// Fuel per call: about 8 ms of straight-line guest code on the dev box
/// (25 M units per ms measured), within the plan's 10 ms budget. Fuel, not
/// wall time, so the same policy decides the same way on every machine.
pub const FUEL_PER_CALL: u64 = 200_000_000;
/// Linear memory cap per instance.
pub const MEMORY_LIMIT: usize = 32 << 20;
/// Largest component accepted.
pub const MAX_COMPONENT_BYTES: usize = 16 << 20;
/// Log lines kept per call, and bytes kept per line.
pub const LOG_LINES: usize = 32;
pub const LOG_LINE_BYTES: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("reading policy {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("policy is larger than {MAX_COMPONENT_BYTES} bytes")]
    TooLarge,
    #[error("not a wyrdline policy component: {0}")]
    Invalid(String),
}

/// A pre-execution or effect-time decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", content = "reason", rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    Deny(String),
    Ask(String),
}

impl Verdict {
    pub fn reason(&self) -> &str {
        match self {
            Verdict::Allow => "",
            Verdict::Deny(r) | Verdict::Ask(r) => r,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub args_json: String,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub cwd: String,
}

fn empty_object() -> String {
    "{}".into()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub exe: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCtx {
    pub repo_root: String,
    #[serde(default)]
    pub scope_globs: Vec<String>,
    #[serde(default)]
    pub prior_denials: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelRules {
    #[serde(default)]
    pub fs_read_deny: Vec<String>,
    #[serde(default)]
    pub fs_write_allow: Vec<String>,
    #[serde(default)]
    pub exec_deny: Vec<String>,
    #[serde(default)]
    pub egress_allow_cidrs: Vec<String>,
    #[serde(default)]
    pub egress_allow_hosts: Vec<String>,
    #[serde(default)]
    pub egress_allow_ports: Vec<u16>,
}

/// Result of one call into the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome<T> {
    pub value: T,
    pub fuel_used: u64,
    pub log: Vec<String>,
    /// Why the call failed, when it did (`value` is then the fallback).
    pub error: Option<String>,
}

struct State {
    limits: StoreLimits,
    log: Vec<String>,
}

impl bindings::PolicyImports for State {
    fn log(&mut self, msg: String) {
        if self.log.len() >= LOG_LINES {
            return;
        }
        let mut end = msg.len().min(LOG_LINE_BYTES);
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        self.log.push(msg[..end].to_string());
    }
}

impl wit::Host for State {}

/// A loaded policy, shareable across threads.
pub struct Policy {
    engine: Engine,
    pre: bindings::PolicyPre<State>,
    /// Name given at load (usually the file stem).
    pub id: String,
    /// `b3:` + the first 16 hex digits of the component's BLAKE3 hash.
    pub version: String,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("id", &self.id)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// One line naming what went wrong, without the wasm backtrace.
fn err_text(e: &wasmtime::Error) -> String {
    if let Some(t) = e.downcast_ref::<wasmtime::Trap>() {
        return match t {
            wasmtime::Trap::OutOfFuel => "fuel exhausted".into(),
            other => other.to_string(),
        };
    }
    e.to_string().lines().next().unwrap_or_default().to_string()
}

type Raw<T> = (Result<T, String>, u64, Vec<String>);

impl Policy {
    pub fn load_file(path: &Path) -> Result<Self, PolicyError> {
        let bytes = std::fs::read(path).map_err(|source| PolicyError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let id = path
            .file_stem()
            .map_or_else(|| "policy".into(), |s| s.to_string_lossy().into_owned());
        Self::load(&id, &bytes)
    }

    pub fn load(id: &str, bytes: &[u8]) -> Result<Self, PolicyError> {
        if bytes.len() > MAX_COMPONENT_BYTES {
            return Err(PolicyError::TooLarge);
        }
        let invalid = |e: wasmtime::Error| PolicyError::Invalid(format!("{e:#}"));
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.max_wasm_stack(1 << 20);
        let engine = Engine::new(&cfg).map_err(invalid)?;
        let component = Component::new(&engine, bytes).map_err(invalid)?;
        let mut linker = Linker::<State>::new(&engine);
        bindings::Policy::add_to_linker::<_, HasSelf<State>>(&mut linker, |s| s)
            .map_err(invalid)?;
        // WASI and anything else the guest's toolchain pulled in: present
        // so the component links, but every call traps.
        linker
            .define_unknown_imports_as_traps(&component)
            .map_err(invalid)?;
        let pre = linker
            .instantiate_pre(&component)
            .and_then(bindings::PolicyPre::new)
            .map_err(invalid)?;
        let hash = blake3::hash(bytes);
        Ok(Policy {
            engine,
            pre,
            id: id.to_string(),
            version: format!("b3:{}", &hex::encode(hash.as_bytes())[..16]),
        })
    }

    fn call<T>(
        &self,
        f: impl FnOnce(&bindings::Policy, &mut Store<State>) -> wasmtime::Result<T>,
    ) -> Raw<T> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT)
            .table_elements(1 << 16)
            .instances(16)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            State {
                limits,
                log: Vec::new(),
            },
        );
        store.limiter(|s| &mut s.limits);
        if let Err(e) = store.set_fuel(FUEL_PER_CALL) {
            return (Err(err_text(&e)), 0, Vec::new());
        }
        let r = self
            .pre
            .instantiate(&mut store)
            .and_then(|p| f(&p, &mut store))
            .map_err(|e| err_text(&e));
        let fuel_used = FUEL_PER_CALL - store.get_fuel().unwrap_or(0);
        let log = std::mem::take(&mut store.data_mut().log);
        (r, fuel_used, log)
    }

    fn verdict((r, fuel_used, log): Raw<wit::Decision>) -> Outcome<Verdict> {
        match r {
            Ok(d) => Outcome {
                value: match d {
                    wit::Decision::Allow => Verdict::Allow,
                    wit::Decision::Deny(s) => Verdict::Deny(s),
                    wit::Decision::Ask(s) => Verdict::Ask(s),
                },
                fuel_used,
                log,
                error: None,
            },
            Err(e) => Outcome {
                value: Verdict::Deny(format!("policy error: {e}")),
                fuel_used,
                log,
                error: Some(e),
            },
        }
    }

    pub fn on_tool_call(&self, call: &ToolCall, ctx: &SessionCtx) -> Outcome<Verdict> {
        let c = wit::ToolCall {
            tool: call.tool.clone(),
            args_json: call.args_json.clone(),
            adapter: call.adapter.clone(),
            cwd: call.cwd.clone(),
        };
        let x = ctx_wit(ctx);
        Self::verdict(self.call(|p, s| p.call_on_tool_call(s, &c, &x)))
    }

    pub fn on_effect(&self, effect: &Effect, ctx: &SessionCtx) -> Outcome<Verdict> {
        let e = wit::Effect {
            kind: effect.kind.clone(),
            path: effect.path.clone(),
            host: effect.host.clone(),
            port: effect.port,
            exe: effect.exe.clone(),
        };
        let x = ctx_wit(ctx);
        Self::verdict(self.call(|p, s| p.call_on_effect(s, &e, &x)))
    }

    /// The session's kernel rules; `value` is `None` when the policy failed,
    /// and the caller must not start an enforced session without them.
    pub fn kernel_rules(&self, ctx: &SessionCtx) -> Outcome<Option<KernelRules>> {
        let x = ctx_wit(ctx);
        let (r, fuel_used, log) = self.call(|p, s| p.call_kernel_rules(s, &x));
        let (value, error) = match r {
            Ok(k) => (
                Some(KernelRules {
                    fs_read_deny: k.fs_read_deny,
                    fs_write_allow: k.fs_write_allow,
                    exec_deny: k.exec_deny,
                    egress_allow_cidrs: k.egress_allow_cidrs,
                    egress_allow_hosts: k.egress_allow_hosts,
                    egress_allow_ports: k.egress_allow_ports,
                }),
                None,
            ),
            Err(e) => (None, Some(e)),
        };
        Outcome {
            value,
            fuel_used,
            log,
            error,
        }
    }
}

fn ctx_wit(ctx: &SessionCtx) -> wit::SessionCtx {
    wit::SessionCtx {
        repo_root: ctx.repo_root.clone(),
        scope_globs: ctx.scope_globs.clone(),
        prior_denials: ctx.prior_denials,
    }
}
