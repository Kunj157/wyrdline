# Ledger format — `wyrdline.ledger.v1`

One SQLite database per session (WAL mode). Decided in
[ADR-004](adr/004-per-session-hash-chained-ledger.md).

## Tables

| Table | Role |
|---|---|
| `meta(key, value)` | `format` = `wyrdline.ledger.v1`, `event_schema` = `wyrdline.event.v1`, `session_id` |
| `events(seq, kind, plane, ts_mono_ns, pid, event_cbor, prev_hash, hash)` | The chain. `seq` is dense from 1. `kind`, `plane`, `ts_mono_ns`, `pid` are indexes copied from the event and checked by `verify`. |
| `seal(id=1, head_seq, head_hash, public_key, signature, sealed_at_ms)` | Written once when the session closes. |
| `tool_calls`, `checkpoints`, `divergences` | Projections, rebuilt by `reindex`; never trusted by `verify`. |

Triggers abort `UPDATE`/`DELETE` on `events` and `seal`, and any `INSERT`
into `events` once a seal exists.

## Hashes

```
event_cbor = canonical_cbor(event)            # wyrd-schema, includes seq, times, process, kind, payload
hash(1)    = BLAKE3("wyrdline.ledger.v1/event" ‖ 0x00×32 ‖ event_cbor(1))
hash(n)    = BLAKE3("wyrdline.ledger.v1/event" ‖ hash(n-1) ‖ event_cbor(n))
```

Stored as raw 32-byte blobs; displayed as `b3:` + lowercase hex.

## Seal

```
message   = "wyrdline.ledger.v1/seal" ‖ u64le(len(session_id)) ‖ session_id ‖ u64le(head_seq) ‖ head_hash
signature = Ed25519(host_key, message)
```

The host key is a 32-byte seed in `$XDG_DATA_HOME/wyrdline/keys/host.key`
(mode 0600, directory 0700); keys readable by group or others are refused.
The signer fingerprint is the first 8 bytes of `BLAKE3(public_key)` in hex.

## Verification (`wyrd verify`)

In order, stopping at the first problem:

1. `seq` dense from 1 → `sequence_gap`
2. `prev_hash` equals the previous row's hash (genesis for seq 1) → `broken_link`
3. stored `hash` equals the recomputed hash → `hash_mismatch`
4. `event_cbor` decodes and agrees with `seq`, `kind`, `plane` → `bad_event`
5. if sealed: seal head equals the last row → `seal_head_mismatch`
6. if sealed: signature verifies → `bad_signature`

Exit code 3 on any problem. Verification is tamper *evidence*: whoever
holds the host private key can rewrite and re-sign history (see the threat
model).
