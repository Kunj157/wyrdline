# Bundle format — `wyrdline.bundle`, version 1

A bundle (`.wyrd`) is one session, redacted and signed, in a single file
that can be shared and opened without the machine that recorded it.
Written by `wyrd export`; the implementation is `crates/wyrd-bundle`
(`FORMAT` = `wyrdline.bundle`, `FORMAT_VERSION` = 1).

## Writing one

```sh
wyrd export <session> -o session.wyrd                  # strict redaction, content included
wyrd export <session> -o session.wyrd --redact standard
wyrd export <session> -o meta.wyrd --no-content        # no cas/: metadata only
wyrd export <session> -o session.wyrd --json           # the summary as JSON
```

| Flag | Meaning |
|---|---|
| `-o, --output FILE` | the bundle to write (atomically: a temporary file renamed into place) |
| `--redact standard\|strict` | redaction applied to every string of every event; `strict` is the default ([redaction](#redaction)) |
| `--no-content` | leave out checkpoint content (`cas/`) |
| `--json` | print the summary as JSON |

`export` refuses a session that is still recording (exit 4: let the
agent exit or `wyrd stop` it first) and a ledger that does not verify
(exit 3: run `wyrd verify`). It signs with the host key, creating one if
none exists yet. The summary names the output, the number of events,
chunks, and withheld files, the ledger and bundle sizes, the manifest
hash, and the signer fingerprint to pass on to the receiver.

A session need not be closed to be exported. The bundle's integrity
comes from the manifest signature over the redacted chain, not from the
ledger seal, which is not included: `ledger.head_hash` is the head of
the events as exported.

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
annotations.automerge  # reserved for annotation sync; not written by this version
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

## Reading and verification

A bundle is untrusted input. `wyrd-bundle`'s reader (also used by
`wyrd verify x.wyrd`, `wyrd import`, and the Studio through WebAssembly)
streams the zstd frame and the archive and checks, in this order, before
it exposes anything:

1. **Archive** — every header is a checksummed ustar header of a regular
   file with a valid v1 path. Rejected, each with its own error: `..`
   components, absolute paths, symbolic and hard links, devices and fifos,
   directories and extended headers, a duplicate entry, data after the end
   of the archive. Limits: entries, bytes per entry and for the manifest,
   total decompressed bytes, compression ratio (size bombs), and the zstd
   window. Buffers grow with the data actually decompressed.
2. **Signature** — the first two entries must be `manifest.json` and
   `signature.ed25519`; the signature is verified before the manifest is
   parsed.
3. **Manifest** — known `format` and `format_version`, no unknown field,
   signer equal to the signature's key, file list sorted, within the
   limits, with the required files, consistent with `content`.
4. **Files** — each further entry is the next file of the manifest, with
   its size and BLAKE3; a chunk must also hash to its name. A ledger that
   does not match its hash is walked to name the modified `seq`.
5. **Chain** — every record decodes, its event is canonical and has the
   next `seq`, its hash is the recomputed chain hash, and the final count
   and head equal `ledger`.

`wyrd verify` prints each step as passed, failed, or skipped and exits 3
on any failure. The signature proves integrity since signing by the key
shown; trusting that key is the receiver's decision.

`wyrd import` adds a verified bundle as a read-only session: the ledger is
rebuilt from the events (same chain head), chunks go into the local CAS,
and `import.json` records where it came from.

The reader, the ledger CBOR decoders, and the manifest decoder are fuzzed
(`make fuzz`, targets in `fuzz/`, seeds in `fuzz/seeds/` regenerated by
`make fuzz-seeds`): 60 s per target on every pull request, 10 minutes per
target nightly.

## Versioning

`format_version` is 1 (`wyrd_bundle::format::FORMAT_VERSION`). Readers
refuse any other version and any unknown manifest field. A change to the container, the layout, the manifest, the
signed message, or the record encoding increments the version; the
`BREAKING CHANGE` footer of the commit says so.
