//! Full ledger verification.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::chain::{GENESIS, chain_hash, hash_from_slice};
use crate::key::{fingerprint, verify_signature};
use crate::{FORMAT_ID, Hash, Ledger, Result, seal_message};

/// The first problem found, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "problem", rename_all = "snake_case")]
#[schemars(rename = "ChainProblem")]
#[ts(rename = "ChainProblem")]
pub enum Problem {
    /// `seq` values are not dense starting at 1.
    SequenceGap { expected: u64, found: u64 },
    /// Stored `prev_hash` does not match the previous row's hash.
    BrokenLink { seq: u64 },
    /// Stored `hash` does not match the recomputed hash of the row.
    HashMismatch { seq: u64 },
    /// The event bytes do not decode, or the indexed columns disagree with them.
    BadEvent { seq: u64, reason: String },
    /// The seal does not refer to the last event.
    SealHeadMismatch { sealed_seq: u64, actual_seq: u64 },
    /// The seal signature does not verify.
    BadSignature,
}

/// Verification result.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema, ts_rs::TS)]
pub struct Verification {
    pub session_id: String,
    pub events: u64,
    pub head_hash: String,
    pub sealed: bool,
    pub signer_fingerprint: Option<String>,
    pub problem: Option<Problem>,
}

impl Verification {
    pub fn ok(&self) -> bool {
        self.problem.is_none()
    }
}

/// Recompute the chain and check the seal. Opens the file read-only and
/// trusts nothing stored except the raw rows.
pub fn verify(path: &Path) -> Result<Verification> {
    let ledger = Ledger::open_read_only(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let fmt: String = conn.query_row("SELECT value FROM meta WHERE key='format'", [], |r| {
        r.get(0)
    })?;
    debug_assert_eq!(fmt, FORMAT_ID);

    let mut stmt = conn
        .prepare("SELECT seq, kind, plane, event_cbor, prev_hash, hash FROM events ORDER BY seq")?;
    let mut rows = stmt.query([])?;
    let mut expected: u64 = 1;
    let mut prev: Hash = GENESIS;
    let mut problem = None;
    while let Some(row) = rows.next()? {
        let seq = u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0);
        let kind: String = row.get(1)?;
        let plane: String = row.get(2)?;
        let cbor: Vec<u8> = row.get(3)?;
        let stored_prev = hash_from_slice(&row.get::<_, Vec<u8>>(4)?);
        let stored_hash = hash_from_slice(&row.get::<_, Vec<u8>>(5)?);
        if seq != expected {
            problem = Some(Problem::SequenceGap {
                expected,
                found: seq,
            });
            break;
        }
        if stored_prev != Some(prev) {
            problem = Some(Problem::BrokenLink { seq });
            break;
        }
        let computed = chain_hash(&prev, &cbor);
        if stored_hash != Some(computed) {
            problem = Some(Problem::HashMismatch { seq });
            break;
        }
        match wyrd_schema::from_canonical_cbor(&cbor) {
            Ok(ev) if ev.seq == seq && ev.kind() == kind && ev.plane().as_str() == plane => {}
            Ok(_) => {
                problem = Some(Problem::BadEvent {
                    seq,
                    reason: "indexed columns disagree with event".into(),
                });
                break;
            }
            Err(e) => {
                problem = Some(Problem::BadEvent {
                    seq,
                    reason: e.to_string(),
                });
                break;
            }
        }
        prev = computed;
        expected += 1;
    }
    let events = expected - 1;

    let seal = ledger.read_seal()?;
    if problem.is_none()
        && let Some(s) = &seal
    {
        if s.head_seq != events || s.head_hash != prev {
            problem = Some(Problem::SealHeadMismatch {
                sealed_seq: s.head_seq,
                actual_seq: events,
            });
        } else {
            let msg = seal_message(ledger.session_id(), s.head_seq, &s.head_hash);
            if !verify_signature(&s.public_key, &msg, &s.signature) {
                problem = Some(Problem::BadSignature);
            }
        }
    }
    Ok(Verification {
        session_id: ledger.session_id().to_string(),
        events,
        head_hash: format!("b3:{}", hex::encode(prev)),
        sealed: seal.is_some(),
        signer_fingerprint: seal.as_ref().map(|s| fingerprint(&s.public_key)),
        problem,
    })
}
