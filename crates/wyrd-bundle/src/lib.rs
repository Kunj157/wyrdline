//! Sharing a session: secret redaction and signed `.wyrd` bundles.
//!
//! Redaction lives here because it guards every path by which data leaves
//! its source: ingest into the ledger (`Standard`) and bundle export
//! (`Strict`). A bundle (see `docs/bundle-format.md`) is a deterministic
//! zstd-compressed ustar archive ([`archive`]).

pub mod archive;
pub mod redact;
