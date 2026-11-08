//! `wyrd show` and `wyrd tail`.

use std::io::Write as _;
use std::time::Duration;

use wyrd_cell::{DataDir, Session};
use wyrd_schema::{Event, EventBody, SessionState};

use super::session::print_event;
use crate::exit;

#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// Session id or unique prefix.
    session: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct TailArgs {
    /// Session id or unique prefix.
    session: String,
    /// Start after this seq (default: the current end, i.e. only new events).
    #[arg(long)]
    after: Option<u64>,
    /// One JSON event per line.
    #[arg(long)]
    json: bool,
}

/// How often `tail` polls the ledger.
const POLL: Duration = Duration::from_millis(250);

pub fn show(a: &ShowArgs) -> anyhow::Result<u8> {
    let s = Session::open(&DataDir::from_env(), &a.session)?;
    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
    let (head_seq, head) = ledger.head();
    let head_hash = format!("b3:{}", hex::encode(head));
    let check = wyrd_ledger::verify(&s.dir().ledger())?;
    let rec = s.record();
    let mut o = std::io::stdout().lock();
    if a.json {
        let out = serde_json::json!({
            "session": rec,
            "head_seq": head_seq,
            "head_hash": head_hash,
            "ok": check.ok(),
            "verification": check,
        });
        serde_json::to_writer_pretty(&mut o, &out)?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    writeln!(
        o,
        "session   {}{}",
        rec.id,
        rec.name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default()
    )?;
    writeln!(
        o,
        "status    {}",
        serde_json::to_value(rec.status)?.as_str().unwrap_or("?")
    )?;
    writeln!(
        o,
        "agent     {}  mode {}",
        rec.agent,
        serde_json::to_value(rec.mode)?.as_str().unwrap_or("?")
    )?;
    writeln!(o, "workspace {}", rec.workspace.display())?;
    writeln!(o, "command   {}", rec.command.join(" "))?;
    writeln!(o, "events    {head_seq}  head {head_hash}")?;
    writeln!(
        o,
        "chain     {}{}",
        if check.ok() {
            "✓ intact"
        } else {
            "✗ broken"
        },
        if check.sealed {
            ", sealed"
        } else {
            ", not sealed"
        }
    )?;
    writeln!(
        o,
        "branch    {} of {}",
        rec.current_branch,
        rec.branches.len()
    )?;
    for c in &rec.checkpoints {
        writeln!(
            o,
            "  {:<5} branch {:<4} seq {:>6}  {} files",
            c.id, c.branch, c.seq, c.files
        )?;
    }
    Ok(exit::OK)
}

pub fn tail(a: &TailArgs) -> anyhow::Result<u8> {
    let data = DataDir::from_env();
    let id = Session::open(&data, &a.session)?.record().id.clone();
    let mut o = std::io::stdout().lock();
    let mut after = if let Some(n) = a.after {
        n
    } else {
        let s = Session::open(&data, &id)?;
        wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?
            .head()
            .0
    };
    // The state the ledger has reached, from the last state change read.
    // Unknown until a change is printed or looked up.
    let mut latest: Option<SessionState> = None;
    let mut latest_known = false;
    loop {
        let s = Session::open(&data, &id)?;
        let ended = matches!(
            s.record().status,
            SessionState::Closed | SessionState::Failed
        );
        let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
        let batch = ledger.events_after(after, 1_000)?;
        for ev in &batch {
            print_event(&mut o, ev, a.json)?;
            after = ev.seq;
            if let Some(to) = state_change_to(ev) {
                latest = Some(to);
                latest_known = true;
            }
        }
        o.flush()?;
        if !batch.is_empty() {
            continue;
        }
        if !ended {
            std::thread::sleep(POLL);
            continue;
        }
        if !latest_known {
            latest = latest_state_up_to(&ledger, after)?;
            latest_known = true;
        }
        // The record is saved before the change that announces the state is
        // appended. Finish when the ledger has reached the record's state.
        if latest == Some(s.record().status) {
            return Ok(exit::OK);
        }
        // Otherwise a close may be in progress. It holds the session lock until
        // it has appended its change, so wait while the lock is held. Once it is
        // free, any close has finished (its change is committed) or crashed (no
        // change will come): read what is left, and finish.
        if wyrd_cell::state::is_locked(&s.dir().lock())? {
            std::thread::sleep(POLL);
            continue;
        }
        loop {
            let rest = ledger.events_after(after, 1_000)?;
            if rest.is_empty() {
                break;
            }
            for ev in &rest {
                print_event(&mut o, ev, a.json)?;
                after = ev.seq;
            }
        }
        o.flush()?;
        return Ok(exit::OK);
    }
}

/// The state a state change reaches, if `ev` is one.
fn state_change_to(ev: &Event) -> Option<SessionState> {
    match &ev.body {
        EventBody::SessionStateChanged(c) => Some(c.to),
        _ => None,
    }
}

/// The state of the last state change at or before `upto`.
fn latest_state_up_to(
    ledger: &wyrd_ledger::Ledger,
    upto: u64,
) -> anyhow::Result<Option<SessionState>> {
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
            if let Some(to) = state_change_to(ev) {
                latest = Some(to);
            }
        }
        cursor = last.seq;
    }
}
