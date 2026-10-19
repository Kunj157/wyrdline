//! Pending `ask` decisions: a tool call waits here until a human allows or
//! denies it (through `wyrd ask` or Studio), or the timeout denies it.
//!
//! Answers come over the intent socket, which the agent can also reach, so
//! every answer carries the session's control token: a random value kept
//! in a 0600 file in the session directory, which the agent's Cell cannot
//! read (plan §17.8).

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Session file holding the control token (0600), present while the
/// session runs.
pub const CONTROL_TOKEN_FILE: &str = "control.token";

/// Default wait for an answer. Below the hook client's and the gateway's
/// own timeouts, so the agent always gets a decision.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// One question waiting for a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
pub struct PendingAsk {
    pub id: u64,
    pub call_id: String,
    pub tool: String,
    /// Redacted tool input as JSON.
    pub args: String,
    /// Why the policy asks.
    pub reason: String,
    /// Policy that asked.
    pub policy_id: String,
    /// Milliseconds until the call is denied for lack of an answer.
    pub expires_in_ms: u64,
}

/// How an ask ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Allow { by: String },
    Deny { by: String },
    TimedOut,
}

#[derive(Debug)]
struct Entry {
    ask: PendingAsk,
    deadline: Instant,
    answer: Option<Answer>,
}

/// Registry of pending asks of one session.
#[derive(Debug)]
pub struct Asks {
    token: String,
    timeout: Duration,
    state: Mutex<(u64, BTreeMap<u64, Entry>)>,
    changed: Condvar,
}

impl Asks {
    pub fn new(token: String, timeout: Duration) -> Self {
        Asks {
            token,
            timeout,
            state: Mutex::new((0, BTreeMap::new())),
            changed: Condvar::new(),
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Constant-time comparison with the session's control token.
    pub fn authorised(&self, token: &str) -> bool {
        let (a, b) = (self.token.as_bytes(), token.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// Register an ask and block until it is answered or times out.
    pub fn ask(
        &self,
        call_id: &str,
        tool: &str,
        args: &str,
        reason: &str,
        policy_id: &str,
    ) -> Answer {
        let deadline = Instant::now() + self.timeout;
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.0 += 1;
        let id = st.0;
        st.1.insert(
            id,
            Entry {
                ask: PendingAsk {
                    id,
                    call_id: call_id.into(),
                    tool: tool.into(),
                    args: args.into(),
                    reason: reason.into(),
                    policy_id: policy_id.into(),
                    expires_in_ms: 0,
                },
                deadline,
                answer: None,
            },
        );
        self.changed.notify_all();
        loop {
            if let Some(a) = st.1.get(&id).and_then(|e| e.answer.clone()) {
                st.1.remove(&id);
                return a;
            }
            let now = Instant::now();
            if now >= deadline {
                st.1.remove(&id);
                return Answer::TimedOut;
            }
            st = self
                .changed
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Asks still waiting, oldest first.
    pub fn pending(&self) -> Vec<PendingAsk> {
        let now = Instant::now();
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.1.values()
            .filter(|e| e.answer.is_none() && e.deadline > now)
            .map(|e| PendingAsk {
                expires_in_ms: u64::try_from((e.deadline - now).as_millis()).unwrap_or(u64::MAX),
                ..e.ask.clone()
            })
            .collect()
    }

    /// Answer ask `id`; false when it is unknown, answered, or expired.
    pub fn answer(&self, id: u64, allow: bool, by: &str) -> bool {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(e) = st.1.get_mut(&id) else {
            return false;
        };
        if e.answer.is_some() || e.deadline <= Instant::now() {
            return false;
        }
        let by = by.to_string();
        e.answer = Some(if allow {
            Answer::Allow { by }
        } else {
            Answer::Deny { by }
        });
        self.changed.notify_all();
        true
    }
}

/// A fresh control token: 32 random bytes, hex.
pub fn new_token() -> std::io::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(hex::encode(b))
}
