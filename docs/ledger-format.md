# Ledger format — `wyrdline.ledger.v1`

One SQLite database per session (WAL mode), at
`sessions/<session id>/ledger.wyrdledger` in the data directory
(`$WYRD_DATA_DIR`, else `$XDG_DATA_HOME/wyrdline`, else
`~/.local/share/wyrdline`). Decided in
[ADR-004](adr/004-per-session-hash-chained-ledger.md); the
implementation is `crates/wyrd-ledger`, the event types are
`crates/wyrd-schema` (`wyrdline.event.v1`, version 1).

The ledger is the product's evidence: every plane of a session (what
the agent said, what the kernel saw, checkpoints, policy decisions,
verdicts, session metadata) is one append-only, hash-chained stream,
sealed with the host's signature when the session closes. Everything
else Wyrdline shows is a projection of it.

## Tables

| Table | Role |
|---|---|
| `meta(key, value)` | `format` = `wyrdline.ledger.v1`, `event_schema` = `wyrdline.event.v1`, `session_id` |
| `events(seq, kind, plane, ts_mono_ns, pid, event_cbor, prev_hash, hash)` | The chain. `seq` is dense from 1. `kind`, `plane`, `ts_mono_ns`, `pid` are indexes copied from the event and checked by `verify`. |
| `seal(id=1, head_seq, head_hash, public_key, signature, sealed_at_ms)` | Written once when the session closes. |
| `tool_calls`, `checkpoints`, `divergences` | Projections, rebuilt by `reindex`; never trusted by `verify`. |

Triggers abort `UPDATE`/`DELETE` on `events` and `seal`, and any `INSERT`
into `events` once a seal exists. All tables are `STRICT`.

```sql
CREATE TABLE events (
    seq         INTEGER PRIMARY KEY,
    kind        TEXT    NOT NULL,     -- e.g. effect.file.write
    plane       TEXT    NOT NULL,     -- meta | intent | effect | state | guard | verdict
    ts_mono_ns  INTEGER NOT NULL,
    pid         INTEGER,
    event_cbor  BLOB    NOT NULL,     -- canonical CBOR of the whole event
    prev_hash   BLOB    NOT NULL,     -- 32 bytes
    hash        BLOB    NOT NULL      -- 32 bytes
) STRICT;
```

A single writer (the `wyrd run` process that owns the live session)
appends; `wyrdd` and the CLI open the file read-only, which WAL readers
can do without blocking the writer.

## Events

Each `event_cbor` is the canonical CBOR of one `Event`
([JSON Schema](schemas/event.v1.json), generated from the Rust types;
the TypeScript types in `packages/schemas` come from the same source):

| Field | Type | Meaning |
|---|---|---|
| `seq` | u64 | dense sequence number, from 1 |
| `ts_mono_ns` | u64 | `CLOCK_MONOTONIC` nanoseconds, from the kernel (effects) or the daemon |
| `ts_wall_ms` | i64 | wall-clock milliseconds since the epoch, for display only |
| `process` | `{ pid, tid, ppid, start_ns, cgroup_id }` or null | the process that caused the event; `(pid, start_ns)` is unique, so a reused PID is a different process |
| `drops_since_last` | u32 | ring-buffer drops the sensor counted since the previous event; non-zero means effects are missing here |
| `body` | object | `kind` plus a kind-specific `payload` |

Field order and types are part of the on-disk format: a change to
them is a schema version bump (`SCHEMA_VERSION`) and a `BREAKING
CHANGE`. Unknown fields are rejected on decode.

### Kinds by plane

| Plane | Kind | Payload (short) |
|---|---|---|
| meta | `meta.session.started` | session id, adapter (`agent_kind`), redacted command, workspace, mode (`cell` / `observe`), kernel release, the `wyrd doctor` capability map, policy hash |
| | `meta.session.state_changed` | the session [state machine](architecture/overview.md#session-lifecycle) |
| | `meta.sensor.lost`, `meta.sensor.resumed` | the session is Degraded between them |
| | `meta.rewind.requested`, `meta.rewind.confirmed`, `meta.rewind.rejected` | a rewind asked for by the agent, decided by a human |
| | `meta.session.closed` | reason, exit code; the last event before the seal |
| intent | `intent.message` | a user prompt or an assistant text, with its turn |
| | `intent.tool_call.started` | `call_id`, `source` (`hook` / `mcp` / `otlp`), tool, redacted `args`, turn |
| | `intent.tool_call.finished` | `call_id`, status, exit code, short redacted summary, duration |
| | `intent.tool_call.denied` | `call_id`, policy, reason |
| | `intent.mcp.request`, `intent.mcp.response` | MCP traffic through the gateway, redacted |
| effect | `effect.proc.exec`, `effect.proc.fork`, `effect.proc.exit` | program and argv (the first 8 arguments, up to 63 bytes each; a 64-byte kernel buffer with its terminator), child pid, exit code |
| | `effect.file.open_write`, `effect.file.write`, `effect.file.rename`, `effect.file.unlink`, `effect.file.mkdir`, `effect.file.chmod`, `effect.file.truncate` | the path (and the second path for renames), whether it is inside the Cell |
| | `effect.file.read_sensitive` | a read of a secret-classified path (path only; contents are never captured) |
| | `effect.net.connect` | family, address, port, the most recent DNS name that resolved to the address in this session, verdict (`attempt` / `allowed` / `denied`) |
| | `effect.net.dns_query` | the name |
| state | `state.checkpoint.created` | checkpoint id, branch, parent, the tool call it follows, the CAS manifest hash (`b3:…`), files changed, bytes added, duration |
| | `state.checkpoint.rewound`, `state.checkpoint.forked`, `state.checkpoint.applied` | the branch operations |
| guard | `guard.decision` | stage (`hook` / `gateway` / `kernel` / `effect`), decision (`allow` / `deny` / `ask`), policy id and version, reason, the call or the effect `seq` it is about |
| verdict | `verdict.claim.extracted` | claim id, turn, type, subject, the verbatim quote, extractor (`rule` / `llm`) |
| | `verdict.divergence.raised` | divergence id, rule (`D001`–`D010`), severity, the claim, the evidence `seq`s, confidence in basis points, summary |
| | `verdict.divergence.updated` | status (`open` / `acknowledged` / `false_positive`), note |

Every string in a payload is redacted at the `standard` level before
the event is appended ([redaction](architecture/redaction.md)).

## Hashes

```
event_cbor = canonical_cbor(event)            # wyrd-schema, includes seq, times, process, kind, payload
hash(1)    = BLAKE3("wyrdline.ledger.v1/event" ‖ 0x00×32 ‖ event_cbor(1))
hash(n)    = BLAKE3("wyrdline.ledger.v1/event" ‖ hash(n-1) ‖ event_cbor(n))
```

Stored as raw 32-byte blobs; displayed as `b3:` + lowercase hex. The
same chain function is used over the redacted events of a bundle
([bundle format](bundle-format.md)).

## Seal

```
message   = "wyrdline.ledger.v1/seal" ‖ u64le(len(session_id)) ‖ session_id ‖ u64le(head_seq) ‖ head_hash
signature = Ed25519(host_key, message)
```

The host key is a 32-byte seed in `$XDG_DATA_HOME/wyrdline/keys/host.key`
(mode 0600, directory 0700), created on first use; keys readable by
group or others are refused. The signer fingerprint is the first 8 bytes
of `BLAKE3(public_key)` in hex. `wyrd close <session>` writes
`meta.session.closed` and the seal; after that no event can be
appended (the trigger raises).

## Durability

A ledger is an SQLite database in WAL mode, with `synchronous=NORMAL` for
appends. What is relied on, and what is not:

- **Atomic and consistent (relied on).** Each append is one SQLite
  transaction. After a crash, SQLite recovers to a prefix of the committed
  transactions: no torn event, no partial batch.
- **Not durable at commit (accepted).** With `NORMAL`, a commit is not synced
  when it returns, so the most recent appends can be lost after a power cut.
  The chain stays valid; the lost tail is simply absent. A checkpoint's event
  is appended after its chunks are durable (see `cas-format.md`), so a lost
  event leaves only unreferenced chunks.
- **Creation (synced).** `Ledger::create` commits the schema with
  `synchronous=FULL`, so the file is never left empty; the session directory is
  then synced to make the database file's name durable.
- **Import (synced).** `Ledger::sync` sets `synchronous=FULL` and runs a full
  WAL checkpoint, which moves the events into the database file and resets the
  log. Import calls it before the imported session is made visible.

The tests in `crates/wyrd-ledger/tests/ledger.rs` check that `sync` moves the
events out of the log. They do not check the disk. A power cut is not simulated.

## Verification (`wyrd verify`)

`wyrd verify <session | path to a ledger file> [--json]` checks, in
order, stopping at the first problem:

1. `seq` dense from 1 → `sequence_gap`
2. `prev_hash` equals the previous row's hash (genesis for seq 1) → `broken_link`
3. stored `hash` equals the recomputed hash → `hash_mismatch`
4. `event_cbor` decodes and agrees with `seq`, `kind`, `plane` → `bad_event`
5. if sealed: seal head equals the last row → `seal_head_mismatch`
6. if sealed: signature verifies → `bad_signature`

It prints the session id, the event count, the head hash, whether the
ledger is sealed and by which fingerprint, and the first problem with
its `seq`; exit code 3 on any problem, 0 otherwise. `wyrd show` runs
the same check (`chain ✓ intact, sealed`), the daemon serves it at
`GET /v1/sessions/{sid}/verify`, and `wyrd export` refuses a ledger
that does not verify.

Verification is tamper *evidence*: whoever holds the host private key
can rewrite and re-sign history (see the
[threat model](threat-model/scope.md)). What it proves is that nobody
*else* changed anything after the seal, and at which `seq` a change
happened if one did.

## Projections

`tool_calls`, `checkpoints`, and `divergences` are query-friendly
tables derived from the chain while it is written:

```sql
tool_calls (call_id, tool, source, args_json, started_seq, ended_seq, status, exit_code, duration_ms)
checkpoints(checkpoint_id, seq, branch_id, parent_id, after_call_id, manifest, files_changed, bytes_added)
divergences(divergence_id, seq, rule_id, severity, status, evidence_json, summary, note)
```

They are disposable: `Ledger::reindex` drops and rebuilds them from
`events` (there is no CLI command for it yet), and nothing that
verifies or exports reads them. Schema migrations only ever touch
projections.

## Reading a ledger yourself

`wyrd events <session> [--plane …] [--json]` prints the events;
`--json` gives one event per line, exactly the decoded `event_cbor`,
which is also the input format `wyrd policy backtest` accepts in place
of a session. With `sqlite3`, the indexed columns are enough for most
questions:

```sql
SELECT seq, kind, pid FROM events WHERE plane = 'effect' AND kind LIKE 'effect.net.%';
```

Do not write to a ledger: the triggers will refuse, and a ledger whose
hashes no longer match its rows fails verification at that `seq`.

## Versioning

`format` in `meta` is `wyrdline.ledger.v1`; readers refuse any other
value. The chain hash domain, the seal message, and the canonical CBOR
of `Event` are all part of version 1. A change to any of them, or an
incompatible change to the event schema (`wyrdline.event.v1`), is a
`BREAKING CHANGE` and bumps the version; ledgers of the old version
must still open read-only.
