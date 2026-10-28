//! Versioned Wyrdline event schema (`wyrdline.event.v1`).
//!
//! Every component — daemon, sensor decoder, correlator (native and Wasm),
//! bundle reader, and the Studio through generated TypeScript — uses these
//! types. The ledger hashes the canonical CBOR encoding produced by
//! [`to_canonical_cbor`], so field order and types here are part of the
//! on-disk format: changing them requires a schema version bump.
//!
//! Integer fields are exported to TypeScript as `number`. Values above
//! 2^53 (only monotonic nanosecond timestamps after ~104 days of uptime)
//! lose precision in the browser; ordering always uses `seq`.
#![forbid(unsafe_code)]

mod body;
pub mod chain;
mod codec;

pub use body::*;
pub use codec::{CodecError, from_canonical_cbor, to_canonical_cbor};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifier of this schema, stored in session metadata and bundles.
pub const SCHEMA_ID: &str = "wyrdline.event.v1";

/// Schema version number. Bump on any incompatible change.
pub const SCHEMA_VERSION: u16 = 1;

/// One entry of the session event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Dense sequence number within the session, starting at 1.
    #[ts(type = "number")]
    pub seq: u64,
    /// `CLOCK_MONOTONIC` nanoseconds (kernel or daemon clock).
    #[ts(type = "number")]
    pub ts_mono_ns: u64,
    /// Wall-clock milliseconds since the Unix epoch, for display only.
    #[ts(type = "number")]
    pub ts_wall_ms: i64,
    /// Process that caused the event, when there is one.
    pub process: Option<ProcessRef>,
    /// Ring-buffer drops observed by the sensor since the previous event.
    pub drops_since_last: u32,
    /// Kind-specific content.
    pub body: EventBody,
}

impl Event {
    /// Plane this event belongs to (derived from its kind).
    pub fn plane(&self) -> Plane {
        self.body.plane()
    }

    /// Dotted kind name, e.g. `effect.file.write`.
    pub fn kind(&self) -> &'static str {
        self.body.kind()
    }
}

/// Identity of a process, robust against PID reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ProcessRef {
    pub pid: u32,
    pub tid: u32,
    pub ppid: u32,
    /// Process start time in monotonic nanoseconds; `(pid, start_ns)` is unique.
    #[ts(type = "number")]
    pub start_ns: u64,
    #[ts(type = "number")]
    pub cgroup_id: u64,
}

/// The six planes of the event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    Meta,
    Intent,
    Effect,
    State,
    Guard,
    Verdict,
}

impl Plane {
    pub const ALL: [Plane; 6] = [
        Plane::Meta,
        Plane::Intent,
        Plane::Effect,
        Plane::State,
        Plane::Guard,
        Plane::Verdict,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Plane::Meta => "meta",
            Plane::Intent => "intent",
            Plane::Effect => "effect",
            Plane::State => "state",
            Plane::Guard => "guard",
            Plane::Verdict => "verdict",
        }
    }
}
