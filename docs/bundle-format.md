# Bundle format — `wyrdline.bundle`, version 1

A bundle (`.wyrd`) is one session, redacted and signed, in a single file
that can be shared and opened without the machine that recorded it.
Written by `wyrd export`; the implementation is `crates/wyrd-bundle`.

## Container

One zstd frame containing a POSIX ustar archive. Every entry is a regular
file with a canonical header:

| Field | Value |
|---|---|
| `name` | the entry path (≤ 100 bytes, no `prefix` field) |
| `mode` | `0000644` |
| `uid`, `gid` | `0` |
| `uname`, `gname` | empty |
| `mtime` | `0` |
| `typeflag` | `0` (regular file) |
| `linkname` | empty |

The archive ends with two zero blocks. Equal input therefore always gives
equal bytes: two exports of the same session hash the same. Standard tools
can list a bundle: `tar --zstd -tvf session.wyrd`.

## Layout

Entries appear in this order:

```
manifest.json          # format, session meta, redaction, ledger head, file list
signature.ed25519      # public key ‖ signature over manifest.json
cas/<hex blake3>       # checkpoint content, sorted by name (absent with --no-content)
claims.json
divergences.json
ledger.cbor-seq
annotations.automerge  # reserved for Phase 12 (collaboration); not written yet
```

No other path is valid. A path is relative, has no empty, `.` or `..`
component, no `\` or NUL, and is either one of the fixed names above or
`cas/` followed by exactly 64 lowercase hex digits.

### `manifest.json`

Pretty-printed JSON followed by a newline; unknown fields are an error.

```json
{
  "format": "wyrdline.bundle",
  "format_version": 1,
  "event_schema": "wyrdline.event.v1",
  "session": { "id": "…", "name": "fix ~/shop", "agent": "claude-code",
               "mode": "cell", "status": "closed", "created_ms": 1760000000000 },
  "redaction": "strict",
  "content": { "included": true, "chunks": 42, "withheld_files": 1 },
  "ledger": { "events": 812, "head_hash": "b3:…" },
  "signer": { "public_key": "<64 hex>", "fingerprint": "<16 hex>" },
  "files": [ { "path": "cas/…", "size": 4096, "blake3": "b3:…" }, … ]
}
```

- `files` lists every entry except `manifest.json` and
  `signature.ed25519`, sorted by path, with its size and `b3:` + BLAKE3 hex.
- `content.included` is `false` for `--no-content`; then there are no
  `cas/` entries. `content.withheld_files` counts checkpoint files whose
  content was left out because their path is secret-classified (`.env`,
  keys, credential files; strict redaction only).
- `ledger.head_hash` is the chain head over the bundled events (below).
- `session.name` is redacted at the bundle's level.

### `signature.ed25519`

96 bytes: the signer's 32-byte Ed25519 public key, then the 64-byte
signature of

```
"wyrdline.bundle.v1/manifest" ‖ manifest.json
```

The signer is the host key that also seals ledgers
([ledger format](ledger-format.md)); the prefix keeps the two kinds of
signature apart. The fingerprint is the first 8 bytes of
`BLAKE3(public_key)` in hex. A bundle proves that its content was not
changed after the holder of that key signed it; whether to trust the key
is the receiver's decision (the fingerprint is printed for comparison).

### `ledger.cbor-seq`

A CBOR sequence (RFC 8742), one record per event in `seq` order (dense
from 1):

```
record     = [ event_cbor: bstr, hash: bstr(32) ]
hash(1)    = BLAKE3("wyrdline.ledger.v1/event" ‖ 0x00×32 ‖ event_cbor(1))
hash(n)    = BLAKE3("wyrdline.ledger.v1/event" ‖ hash(n-1) ‖ event_cbor(n))
```

`event_cbor` is the canonical CBOR of the event (`wyrd-schema`) after
redaction, so the chain is recomputed over the redacted events and its
head is `ledger.head_hash`. The chain hash function is the ledger's.

### `claims.json`, `divergences.json`

JSON arrays projected from the bundled events: every `verdict.claim.extracted`
payload, and every divergence with its latest triage state (the same view
as `GET /v1/sessions/{sid}/divergences`). They are conveniences: a reader
can rebuild both from the ledger.

### `cas/`

Chunks of the CAS ([CAS format](cas-format.md)), each named by the hex
BLAKE3 of its bytes: the manifest of every checkpoint in the ledger and
the chunks of its files. With strict redaction, the content of
secret-classified paths is not included (their path and hash stay in the
checkpoint manifest).

## Redaction

Every string of every event payload is redacted again at export:

- `standard`: the ingest rules (known secret formats, tokens, auth
  headers, URL passwords, high-entropy strings).
- `strict` (the default of `wyrd export`): standard, plus home-directory
  user names (`/home/alice` → `~`), and host names replaced by stable
  pseudonyms `host-<hash8>.invalid` — in URLs, after `user@`, in the host
  fields of `effect.net.dns_query` and `effect.net.connect`, and every
  further occurrence of a host name the session resolved, connected to, or
  mentioned in a URL. See [redaction](architecture/redaction.md).

File content in `cas/` is not rewritten (that would break the hashes);
strict redaction withholds secret-classified files instead, and
`--no-content` leaves out all content.

## Versioning

`format_version` is 1. Readers refuse any other version and any unknown
manifest field. A change to the container, the layout, the manifest, the
signed message, or the record encoding increments the version; the
`BREAKING CHANGE` footer of the commit says so.
