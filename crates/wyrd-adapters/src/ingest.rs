//! Turns intent-socket requests into ledger events and pre-execution
//! decisions. Pure apart from the [`Sink`], so it is tested without a
//! socket, a ledger, or a Cell.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use wyrd_bundle::redact::{Level, redact_json, redact_str};
use wyrd_schema::{
    CallStatus, CaptureMode, Decision, EventBody, GuardDecision, GuardStage, IntentSource, Message,
    Role, ToolCallDenied, ToolCallFinished, ToolCallStarted,
};

use crate::asks::{Answer, Asks};
use crate::mcp::{McpState, PINNING_POLICY};
use crate::{Adapter, Direction, Request, Response};

/// Redaction level applied to everything before it reaches the ledger.
pub const INGEST_LEVEL: Level = Level::Standard;

/// An adapter-neutral hook event with raw (not yet redacted) content.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    UserPrompt {
        text: String,
    },
    PreTool {
        call_id: String,
        tool: String,
        args: Value,
    },
    PostTool {
        call_id: String,
        status: CallStatus,
        exit_code: Option<i32>,
        summary: Option<String>,
        duration_ms: u64,
    },
    AssistantMessages {
        texts: Vec<String>,
    },
    Ignored,
}

/// Where ingested events go.
pub trait Sink: Send + Sync {
    /// Append events to the session ledger, in order.
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String>;
    /// A tool call is about to run: take any pending checkpoint first, so
    /// it reflects the previous call only.
    fn before_tool_call(&self) {}
    /// A tool call finished: schedule a checkpoint attributed to it.
    fn after_tool_call(&self, _call_id: &str) {}
}

/// Pre-execution policy until Wasm policies arrive (Phase 9): deny tools by
/// name (`Bash`, or a prefix pattern such as `mcp__github__*`), allow the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaticPolicy {
    pub deny_tools: Vec<String>,
}

impl StaticPolicy {
    pub const ID: &'static str = "wyrdline.static";
    pub const VERSION: &'static str = "1";

    pub fn decide(&self, tool: &str) -> (Decision, String) {
        let hit = self.deny_tools.iter().find(|p| match p.strip_suffix('*') {
            Some(prefix) => tool.starts_with(prefix),
            None => tool == p.as_str(),
        });
        match hit {
            Some(p) => (Decision::Deny, format!("tool {tool} is denied ({p})")),
            None => (Decision::Allow, "no rule denies this tool".into()),
        }
    }
}

/// A pre-execution policy (the Wasm policy of `wyrd run --policy`), kept
/// behind a trait so this crate does not depend on the Wasm runtime.
pub trait ToolPolicy: Send + Sync + std::fmt::Debug {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    /// Decide on a tool call; `args` is already redacted. A failing policy
    /// must return a deny.
    fn on_tool_call(
        &self,
        tool: &str,
        args: &Value,
        adapter: &str,
        prior_denials: u32,
    ) -> (Decision, String);
}

/// Policy id recorded for decisions Wyrdline makes about its own input.
pub const INGEST_POLICY: &str = "wyrdline.ingest";

/// Ingest state of one session.
#[derive(Debug)]
pub struct Ingest<S> {
    sink: S,
    mode: CaptureMode,
    policy: StaticPolicy,
    tool_policy: Option<Arc<dyn ToolPolicy>>,
    asks: Option<Arc<Asks>>,
    denials: AtomicU32,
    turn: Mutex<u32>,
    mcp: Mutex<McpState>,
}

fn guard(
    stage: GuardStage,
    decision: Decision,
    (policy, version): (&str, &str),
    reason: String,
    call_id: Option<String>,
) -> EventBody {
    EventBody::GuardDecision(GuardDecision {
        stage,
        decision,
        policy_id: policy.to_string(),
        policy_version: version.to_string(),
        reason,
        call_id,
        subject_seq: None,
    })
}

impl<S: Sink> Ingest<S> {
    /// `turn` is the last user turn already in the ledger (0 for a new session).
    pub fn new(sink: S, mode: CaptureMode, policy: StaticPolicy, turn: u32) -> Self {
        Ingest {
            sink,
            mode,
            policy,
            tool_policy: None,
            asks: None,
            denials: AtomicU32::new(0),
            turn: Mutex::new(turn),
            mcp: Mutex::new(McpState::default()),
        }
    }

    /// Decide tool calls with `policy` after the static deny list.
    #[must_use]
    pub fn with_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.tool_policy = Some(policy);
        self
    }

    /// Hold `ask` decisions for a human answer; without this, `ask` is
    /// passed through to the agent.
    #[must_use]
    pub fn with_asks(mut self, asks: Arc<Asks>) -> Self {
        self.asks = Some(asks);
        self
    }

    pub fn asks(&self) -> Option<&Arc<Asks>> {
        self.asks.as_ref()
    }

    /// Decide a tool call: the static deny list, then the policy, then a
    /// human for `ask`. Events recorded so far (`bodies`) are appended
    /// before waiting, so the pending call is visible while it waits.
    /// Returns the final decision, its reason, and the deciding policy.
    fn decide(
        &self,
        stage: GuardStage,
        call_id: &str,
        tool: &str,
        args: &Value,
        adapter: &str,
        bodies: &mut Vec<EventBody>,
    ) -> Result<(Decision, String, String), Response> {
        let static_id = (StaticPolicy::ID, StaticPolicy::VERSION);
        let (mut decision, mut reason) = self.policy.decide(tool);
        let mut who = static_id;
        let owned;
        if decision == Decision::Allow
            && let Some(p) = &self.tool_policy
        {
            let prior = self.denials.load(Ordering::Relaxed);
            (decision, reason) = p.on_tool_call(tool, args, adapter, prior);
            owned = (p.id().to_string(), p.version().to_string());
            who = (owned.0.as_str(), owned.1.as_str());
        }
        bodies.push(guard(
            stage,
            decision,
            who,
            reason.clone(),
            Some(call_id.into()),
        ));
        if decision == Decision::Ask
            && let Some(asks) = &self.asks
        {
            self.append(std::mem::take(bodies))?;
            let started = std::time::Instant::now();
            let answer = asks.ask(call_id, tool, &args.to_string(), &reason, who.0);
            let secs = started.elapsed().as_secs_f32();
            (decision, reason) = match answer {
                Answer::Allow { by } => (
                    Decision::Allow,
                    format!("allowed by the user ({by}, {secs:.1} s): {reason}"),
                ),
                Answer::Deny { by } => (
                    Decision::Deny,
                    format!("denied by the user ({by}, {secs:.1} s): {reason}"),
                ),
                Answer::TimedOut => (
                    Decision::Deny,
                    format!("no answer within {} s: {reason}", asks.timeout().as_secs()),
                ),
            };
            bodies.push(guard(
                stage,
                decision,
                who,
                reason.clone(),
                Some(call_id.into()),
            ));
        }
        if decision == Decision::Deny {
            self.denials.fetch_add(1, Ordering::Relaxed);
        }
        Ok((decision, reason, who.0.to_string()))
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Ok,
            Request::Asks { token } => match &self.asks {
                Some(a) if a.authorised(&token) => Response::Asks { asks: a.pending() },
                _ => Response::Error {
                    message: "not authorised".into(),
                },
            },
            Request::Answer {
                token,
                id,
                allow,
                by,
            } => match &self.asks {
                Some(a) if a.authorised(&token) => {
                    if a.answer(id, allow, &by) {
                        Response::Ok
                    } else {
                        Response::Error {
                            message: format!("no pending ask {id}"),
                        }
                    }
                }
                _ => Response::Error {
                    message: "not authorised".into(),
                },
            },
            Request::Hook { adapter, payload } => match Adapter::from_name(&adapter) {
                Some(a) => self.hook(a, &payload),
                None => Response::Error {
                    message: format!("unknown adapter {adapter}"),
                },
            },
            Request::Mcp {
                server,
                direction,
                message,
            } => self.mcp(&server, direction, &message),
            Request::Otlp { request } => {
                let mapped = wyrd_otlp::map_traces(&request, INGEST_LEVEL);
                match self.append(mapped.events) {
                    Ok(()) => Response::Ok,
                    Err(r) => r,
                }
            }
        }
    }

    fn turn(&self) -> u32 {
        *self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A message seen by the MCP gateway. `tools/call` requests are
    /// answered with a decision; everything else with `Ok`.
    fn mcp(&self, server: &str, direction: Direction, message: &Value) -> Response {
        let observed = self
            .mcp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(server, direction, message, std::time::Instant::now());
        let mut bodies = observed.events;
        for tool in &observed.rug_pulls {
            bodies.push(EventBody::GuardDecision(GuardDecision {
                stage: GuardStage::Gateway,
                decision: Decision::Ask,
                policy_id: PINNING_POLICY.into(),
                policy_version: StaticPolicy::VERSION.into(),
                reason: format!(
                    "tool {tool} on MCP server {server} changed its description or schema since it was first listed (possible rug pull)"
                ),
                call_id: None,
                subject_seq: None,
            }));
        }
        let Some((call_id, tool, args)) = observed.call else {
            if let Err(r) = self.append(bodies) {
                return r;
            }
            if let Some(done) = observed.finished {
                self.sink.after_tool_call(&done);
            }
            return Response::Ok;
        };
        self.sink.before_tool_call();
        let turn = self.turn();
        bodies.push(EventBody::ToolCallStarted(ToolCallStarted {
            call_id: call_id.clone(),
            source: IntentSource::Mcp,
            tool: tool.clone(),
            args: args.clone(),
            turn: (turn > 0).then_some(turn),
        }));
        let (decision, reason, policy_id) = match self.decide(
            GuardStage::Gateway,
            &call_id,
            &tool,
            &args,
            "mcp",
            &mut bodies,
        ) {
            Ok(d) => d,
            Err(r) => return r,
        };
        if decision == Decision::Deny {
            bodies.push(EventBody::ToolCallDenied(ToolCallDenied {
                call_id,
                policy_id,
                reason: reason.clone(),
            }));
        }
        if let Err(r) = self.append(bodies) {
            return r;
        }
        Response::Decision {
            decision,
            reason: (decision != Decision::Allow).then_some(reason),
        }
    }

    fn append(&self, bodies: Vec<EventBody>) -> Result<(), Response> {
        self.sink.append(bodies).map_err(|e| {
            tracing::error!(error = %e, "intent ingest could not append to the ledger");
            Response::Error {
                message: format!("ledger: {e}"),
            }
        })
    }

    /// A payload that cannot be understood: record it as a decision about our
    /// own input. Cell mode fails closed, observe mode fails open.
    fn malformed(&self, adapter: Adapter, why: &str) -> Response {
        let decision = match self.mode {
            CaptureMode::Cell => Decision::Deny,
            CaptureMode::Observe => Decision::Allow,
        };
        let reason = format!("malformed {} hook payload: {why}", adapter.name());
        let event = guard(
            GuardStage::Hook,
            decision,
            (INGEST_POLICY, StaticPolicy::VERSION),
            reason.clone(),
            None,
        );
        if let Err(r) = self.append(vec![event]) {
            return r;
        }
        Response::Decision {
            decision,
            reason: Some(reason),
        }
    }

    fn hook(&self, adapter: Adapter, payload: &Value) -> Response {
        let event = match adapter.parse(payload) {
            Ok(e) => e,
            Err(why) => return self.malformed(adapter, &why),
        };
        match event {
            HookEvent::Ignored => Response::Ok,
            HookEvent::UserPrompt { text } => {
                let turn = {
                    let mut t = self
                        .turn
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *t += 1;
                    *t
                };
                self.append(vec![EventBody::Message(Message {
                    role: Role::User,
                    turn,
                    text: redact_str(&text, INGEST_LEVEL),
                })])
                .map_or_else(|r| r, |()| Response::Ok)
            }
            HookEvent::AssistantMessages { texts } => {
                let turn = *self
                    .turn
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let bodies = texts
                    .iter()
                    .map(|t| {
                        EventBody::Message(Message {
                            role: Role::Assistant,
                            turn,
                            text: redact_str(t, INGEST_LEVEL),
                        })
                    })
                    .collect();
                self.append(bodies).map_or_else(|r| r, |()| Response::Ok)
            }
            HookEvent::PreTool {
                call_id,
                tool,
                args,
            } => self.pre_tool(adapter, call_id, &tool, &args),
            HookEvent::PostTool {
                call_id,
                status,
                exit_code,
                summary,
                duration_ms,
            } => {
                let body = EventBody::ToolCallFinished(ToolCallFinished {
                    call_id: call_id.clone(),
                    status,
                    exit_code,
                    summary: summary.map(|s| redact_str(&s, INGEST_LEVEL)),
                    duration_ms,
                });
                if let Err(r) = self.append(vec![body]) {
                    return r;
                }
                self.sink.after_tool_call(&call_id);
                Response::Ok
            }
        }
    }

    fn pre_tool(&self, adapter: Adapter, call_id: String, tool: &str, args: &Value) -> Response {
        self.sink.before_tool_call();
        let turn = *self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let args = redact_json(args, INGEST_LEVEL);
        let mut bodies = vec![EventBody::ToolCallStarted(ToolCallStarted {
            call_id: call_id.clone(),
            source: IntentSource::Hook,
            tool: tool.to_string(),
            args: args.clone(),
            turn: (turn > 0).then_some(turn),
        })];
        let (decision, reason, policy_id) = match self.decide(
            GuardStage::Hook,
            &call_id,
            tool,
            &args,
            adapter.name(),
            &mut bodies,
        ) {
            Ok(d) => d,
            Err(r) => return r,
        };
        if decision == Decision::Deny {
            bodies.push(EventBody::ToolCallDenied(ToolCallDenied {
                call_id,
                policy_id,
                reason: reason.clone(),
            }));
        }
        if let Err(r) = self.append(bodies) {
            return r;
        }
        Response::Decision {
            decision,
            reason: (decision != Decision::Allow).then_some(reason),
        }
    }
}
