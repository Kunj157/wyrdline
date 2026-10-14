# ADR-004: Per-session SQLite ledger with a hash chain

- Status: Accepted
- Date: 2026-10-14

## Context

The ledger is the product's source of truth. It must ingest ≥ 150 k events/s,
survive crashes, be queryable by the daemon and the Studio, and make
tampering detectable after the fact — including tampering by someone with
write access to the file.

## Decision

- One SQLite database (WAL mode) per session, written by a single writer
  task with batched transactions.
- Table `events` is the chain: each row stores
  `hash = BLAKE3(prev_hash ‖ seq ‖ plane ‖ kind ‖ ts_mono_ns ‖ payload_cbor)`.
  `seq` is dense and starts at 1; the genesis `prev_hash` is 32 zero bytes.
- Triggers reject `UPDATE` and `DELETE` on `events`.
- Every other table is a projection rebuilt from `events` (`wyrd reindex`).
- On close, the daemon signs `(session_id, head_seq, head_hash)` with the
  host's ed25519 key. `wyrd verify` recomputes the chain and checks the
  signature, reporting the first bad `seq`.

## Consequences

- Sessions are independent files: easy to export, delete, and back up.
- Tamper evidence, not tamper proofing: an attacker with the host key can
  rewrite history. The bundle signature and Hub ingest verification limit
  what leaves the machine. Documented in the threat model.
- Schema changes touch projections only; the event format is versioned.

## Alternatives considered

- **One global database** — cross-session queries are easier, but deleting,
  exporting, and corruption isolation are harder.
- **Append-only log files** — fastest writes, but every query would need a
  custom index.
- **Merkle tree instead of a chain** — enables inclusion proofs we do not
  need yet; the chain is simpler and verification is linear anyway.
