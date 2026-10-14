//! Turns intent-socket requests into ledger events and pre-execution
//! decisions. Pure apart from the [`Sink`], so it is tested without a
//! socket, a ledger, or a Cell.

use std::sync::Mutex;

use serde_json::Value;
use wyrd_bundle::redact::{Level, redact_json, redact_str};
use wyrd_schema::{
    CallStatus, CaptureMode, Decision, EventBody, GuardDecision, GuardStage, IntentSource, Message,
    Role, ToolCallDenied, ToolCallFinished, ToolCallStarted,
};

use crate::{Adapter, Request, Response};

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

/// Policy id recorded for decisions Wyrdline makes about its own input.
pub const INGEST_POLICY: &str = "wyrdline.ingest";

/// Ingest state of one session.
#[derive(Debug)]
pub struct Ingest<S> {
    sink: S,
    mode: CaptureMode,
    policy: StaticPolicy,
    turn: Mutex<u32>,
}

fn guard(decision: Decision, policy: &str, reason: String, call_id: Option<String>) -> EventBody {
    EventBody::GuardDecision(GuardDecision {
        stage: GuardStage::Hook,
        decision,
        policy_id: policy.to_string(),
        policy_version: StaticPolicy::VERSION.to_string(),
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
            turn: Mutex::new(turn),
        }
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Ok,
            Request::Hook { adapter, payload } => match Adapter::from_name(&adapter) {
                Some(a) => self.hook(a, &payload),
                None => Response::Error {
                    message: format!("unknown adapter {adapter}"),
                },
            },
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
        if let Err(r) = self.append(vec![guard(decision, INGEST_POLICY, reason.clone(), None)]) {
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
            } => self.pre_tool(call_id, tool, &args),
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

    fn pre_tool(&self, call_id: String, tool: String, args: &Value) -> Response {
        self.sink.before_tool_call();
        let turn = *self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (decision, reason) = self.policy.decide(&tool);
        let mut bodies = vec![
            EventBody::ToolCallStarted(ToolCallStarted {
                call_id: call_id.clone(),
                source: IntentSource::Hook,
                tool,
                args: redact_json(args, INGEST_LEVEL),
                turn: (turn > 0).then_some(turn),
            }),
            guard(
                decision,
                StaticPolicy::ID,
                reason.clone(),
                Some(call_id.clone()),
            ),
        ];
        if decision == Decision::Deny {
            bodies.push(EventBody::ToolCallDenied(ToolCallDenied {
                call_id,
                policy_id: StaticPolicy::ID.into(),
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
