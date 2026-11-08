//! Deciding when a reader that follows a session's ledger has read all of it.
//!
//! `Session::close` saves the record as Closed, then appends the state change
//! that announces it, then seals the ledger. A reader that ended on the record
//! alone could stop before that change was appended. The rule here is:
//!
//! - the session has ended when its record reads closed or failed;
//! - it is finished once the latest state change the reader has read matches
//!   that status;
//! - until then, while another process holds the session lock, a close is still
//!   running and the reader waits. The lock is held from saving the record until
//!   the change is appended, so once it is free the close has either finished
//!   (its change is committed) or crashed (no change is coming). Either way the
//!   reader may read what is left and stop.
//!
//! `wyrd tail` and the daemon's event stream both use this.

use wyrd_ledger::Ledger;
use wyrd_schema::{Event, EventBody, SessionState};

use crate::Result;
use crate::session::Session;
use crate::state;

/// What a reader of one session's ledger has reached.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Follow {
    /// The state the last state change the reader has read reaches.
    latest: Option<SessionState>,
    /// Whether `latest` is known. It is not, until a change is read or looked
    /// up.
    known: bool,
}

impl Follow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note an event the reader has been given, in ledger order.
    pub fn observe(&mut self, ev: &Event) {
        if let EventBody::SessionStateChanged(c) = &ev.body {
            self.latest = Some(c.to);
            self.known = true;
        }
    }

    /// Call once the reader has read every event up to `after` and has none
    /// left to send. Returns whether the session has ended and the reader may
    /// stop: it must read whatever remains after `after` first, and stop when
    /// that read is empty. Returns `false` when it should wait and poll again.
    pub fn ended(&mut self, session: &Session, ledger: &Ledger, after: u64) -> Result<bool> {
        let status = session.record().status;
        if !matches!(status, SessionState::Closed | SessionState::Failed) {
            return Ok(false);
        }
        if !self.known {
            self.latest = latest_up_to(ledger, after)?;
            self.known = true;
        }
        if self.latest == Some(status) {
            return Ok(true);
        }
        Ok(!state::is_locked(&session.dir().lock())?)
    }
}

/// The state the last state change at or before `upto` reaches.
fn latest_up_to(ledger: &Ledger, upto: u64) -> Result<Option<SessionState>> {
    let mut latest = None;
    let mut cursor = 0;
    loop {
        let batch = ledger.events_after(cursor, 1_000)?;
        let Some(last) = batch.last() else {
            return Ok(latest);
        };
        for ev in &batch {
            if ev.seq > upto {
                return Ok(latest);
            }
            if let EventBody::SessionStateChanged(c) = &ev.body {
                latest = Some(c.to);
            }
        }
        cursor = last.seq;
    }
}
