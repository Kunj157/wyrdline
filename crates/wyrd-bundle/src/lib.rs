//! Sharing a session: redaction now; `.wyrd` export, import, and signing
//! arrive in Phase 11.
//!
//! Redaction lives here because it guards every path by which data leaves
//! its source: ingest into the ledger (`Standard`) and bundle export
//! (`Strict`).

pub mod redact;
