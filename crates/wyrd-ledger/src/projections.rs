//! Projections: query-friendly tables derived from the event chain.
//! They are disposable; `Ledger::reindex` rebuilds them from scratch.

use rusqlite::{Connection, params};
use wyrd_schema::{Event, EventBody};

pub(crate) const SCHEMA_SQL: &str = r"
CREATE TABLE tool_calls (
    call_id      TEXT PRIMARY KEY,
    tool         TEXT NOT NULL,
    source       TEXT NOT NULL,
    args_json    TEXT NOT NULL,
    started_seq  INTEGER NOT NULL,
    ended_seq    INTEGER,
    status       TEXT,
    exit_code    INTEGER,
    duration_ms  INTEGER
) STRICT;

CREATE TABLE checkpoints (
    checkpoint_id  TEXT PRIMARY KEY,
    seq            INTEGER NOT NULL,
    branch_id      TEXT NOT NULL,
    parent_id      TEXT,
    after_call_id  TEXT,
    manifest       TEXT NOT NULL,
    files_changed  INTEGER NOT NULL,
    bytes_added    INTEGER NOT NULL
) STRICT;

CREATE TABLE divergences (
    divergence_id  TEXT PRIMARY KEY,
    seq            INTEGER NOT NULL,
    rule_id        TEXT NOT NULL,
    severity       TEXT NOT NULL,
    status         TEXT NOT NULL,
    evidence_json  TEXT NOT NULL,
    summary        TEXT NOT NULL,
    note           TEXT
) STRICT;
";

pub(crate) fn clear(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("DELETE FROM tool_calls; DELETE FROM checkpoints; DELETE FROM divergences;")
}

fn as_str<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

#[allow(clippy::cast_possible_wrap)]
pub(crate) fn apply(conn: &Connection, ev: &Event) -> rusqlite::Result<()> {
    let seq = ev.seq as i64;
    match &ev.body {
        EventBody::ToolCallStarted(c) => {
            conn.execute(
                "INSERT OR REPLACE INTO tool_calls (call_id, tool, source, args_json, started_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    c.call_id,
                    c.tool,
                    as_str(&c.source),
                    c.args.to_string(),
                    seq
                ],
            )?;
        }
        EventBody::ToolCallFinished(c) => {
            conn.execute(
                "UPDATE tool_calls SET ended_seq = ?2, status = ?3, exit_code = ?4, duration_ms = ?5
                 WHERE call_id = ?1",
                params![c.call_id, seq, as_str(&c.status), c.exit_code, c.duration_ms as i64],
            )?;
        }
        EventBody::ToolCallDenied(c) => {
            conn.execute(
                "UPDATE tool_calls SET ended_seq = ?2, status = 'denied' WHERE call_id = ?1",
                params![c.call_id, seq],
            )?;
        }
        EventBody::CheckpointCreated(c) => {
            conn.execute(
                "INSERT OR REPLACE INTO checkpoints
                 (checkpoint_id, seq, branch_id, parent_id, after_call_id, manifest,
                  files_changed, bytes_added)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    c.checkpoint_id,
                    seq,
                    c.branch_id,
                    c.parent_id,
                    c.after_call_id,
                    c.manifest,
                    c.files_changed,
                    c.bytes_added as i64
                ],
            )?;
        }
        EventBody::DivergenceRaised(d) => {
            conn.execute(
                "INSERT OR REPLACE INTO divergences
                 (divergence_id, seq, rule_id, severity, status, evidence_json, summary)
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6)",
                params![
                    d.divergence_id,
                    seq,
                    d.rule_id,
                    as_str(&d.severity),
                    serde_json::to_string(&d.evidence_seqs).unwrap_or_default(),
                    d.summary
                ],
            )?;
        }
        EventBody::DivergenceUpdated(d) => {
            conn.execute(
                "UPDATE divergences SET status = ?2, note = ?3 WHERE divergence_id = ?1",
                params![d.divergence_id, as_str(&d.status), d.note],
            )?;
        }
        _ => {}
    }
    Ok(())
}
