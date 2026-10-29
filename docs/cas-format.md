# Content-addressed store — `wyrdline.cas.v1`

Used for Cell checkpoints and, later, bundle content.

## Layout

```
<root>/
├── FORMAT                 # "wyrdline.cas.v1"
├── chunks/ab/cd/<hex>     # zstd(level 3) of the chunk bytes; <hex> = BLAKE3 of the *uncompressed* bytes
└── tmp/                   # staging for atomic writes (write → rename; see Durability)
```

Every read decompresses and re-hashes the chunk; a mismatch is reported as
`corrupt`, an absent file as `missing`.

## Chunking

FastCDC 2020 with normalisation level 1: minimum 16 KiB, average 64 KiB,
maximum 256 KiB. Files are streamed, never loaded whole. Identical content
anywhere in the store is stored once.

## Manifest

A manifest is stored as an ordinary chunk; its hash identifies a checkpoint.
Canonical CBOR of:

```
{
  format: "wyrdline.cas.v1/manifest",
  entries: [                      # sorted by path bytes, unique
    {
      path:     bytes,            # relative, '/'-separated, no '', '.', '..', NUL; ≤ 4096 bytes
      kind:     "file" | "dir" | "symlink" | "whiteout",
      mode:     u32,              # st_mode & 0o7777
      size:     u64,
      mtime_ns: i64,
      chunks:   [bytes32],        # files only
      target:   bytes | null,     # symlinks only
      opaque:   bool              # overlay opaque directory
    }
  ]
}
```

Validation (on build and on every decode): known format, ≤ 2 000 000
entries, sorted and unique, safe path components, **every entry's parent is
a `dir` entry** (so a symlink can never act as a path component when
materialising), symlinks have targets, only files have chunks.

## Snapshot and materialise

- `snapshot(dir, base)` walks without following symlinks. With a base
  manifest, files whose size, mtime, and mode are unchanged reuse the base
  chunk list without being read (incremental checkpoints). Character
  devices 0/0 are recorded as whiteouts; `trusted.overlay.opaque` or
  `user.overlay.opaque` = `y` marks opaque directories. FIFOs, sockets, and
  other devices are skipped and counted.
- `materialize(manifest, dest)` requires an empty or absent destination,
  restores contents, modes, and mtimes (directories last, deepest first),
  and returns whiteouts and opaque directories for the Cell to apply.
  `materialize` does not sync what it writes. A rewind does: it runs one
  `syncfs` on the restored tree before it changes the branch in the session
  record and before it writes the rewind event, so neither ever refers to a
  tree that a crash could leave incomplete.

## Garbage collection

Mark from root manifest hashes, sweep every unreferenced chunk. `dry_run`
reports without deleting.

## Durability

**Invariant:** a file at its chunk name (`chunks/ab/cd/<hex>`) always holds
the complete, verified content for that name. Nothing creates a chunk name
until its data is on disk. A crash can leave a missing name, never a wrong
one, and a missing name is only ever referenced by nothing.

### Checkpoint (`Session::checkpoint`)

1. Inside the freeze, `snapshot_staged` chunks each changed file and writes
   each new chunk to a unique staging name `tmp/<hex>.<pid>.<seq>`. No sync,
   no rename. Files are chunked by up to eight workers, each with its own
   batch, merged afterwards.
2. The cgroup is thawed.
3. `stage_manifest` writes the manifest to staging the same way.
4. `commit`:
   1. `syncfs` on the store filesystem (barrier 1): every staged file's data
      is on disk;
   2. each staged file is renamed to its chunk name;
   3. `syncfs` again (barrier 2): those names are on disk.
5. Only then is the `CheckpointCreated` event appended to the ledger.

A crash at any point leaves at most unreferenced chunks and staging files.
Barrier 2 is kept even though a journal commit would usually make the renames
durable: that depends on the filesystem, so the ordering does not rely on it.

### Every write checks what it finds

When a chunk name already exists, it is read and its hash checked before it
is trusted (`verified`). A chunk that is missing or fails the check is written
again: it is staged, and its rename replaces the bad file in the same commit,
so both barriers cover the replacement. Nothing is deleted before the new
file is in place.

Durable callers (`put`, `put_stream`, `put_manifest`, `snapshot`, bundle
import) write one chunk on their own, in this order:

1. Create any missing fan-out directories (`chunks/ab`, `chunks/ab/cd`). After
   each new directory is made, its parent is synced before anything is written
   under it. A directory another process made first is synced the same way.
2. Write the compressed chunk to `tmp/` and sync its data.
3. Rename it to its chunk name.
4. Sync the chunk's directory, so the new name is on disk.

A chunk that already verifies is synced by its directory, which covers a
rename made by another batch that is still waiting for its second barrier.
Staged writes (the checkpoint path) create their directories without syncing
them: the first `syncfs` of the commit covers them, before any rename.

Opening a store applies the same rule to `chunks/` and `tmp/`. The `FORMAT`
marker is written to a staging file, synced, renamed into place, and its
directory synced.

The tests in `crates/wyrd-cas/tests/durable_order.rs` record these steps,
across threads, and check the order: every created directory is synced in its
parent (or covered by a `syncfs`) before a file is renamed into it, and every
rename is followed by a sync of its directory. A power cut is not simulated;
the order is checked, not the disk.

### Staging files after a crash

`Cas::open` removes staging files whose process is no longer running. A file
whose process is alive is kept: it may be a write in progress, and the store
is shared by sessions. Nothing reads a staging file: chunks are only ever
read from their names.

### What is tested and what is argued

- Tested: staged chunks are invisible until commit; a dropped batch leaves
  no chunk; a zero-length or truncated chunk at a name is written again, both
  at the store level and through a checkpoint that then restores byte for
  byte; leftover staging files of a dead process are removed and a live one
  is kept; an unsynced-then-committed tree matches the durable snapshot.
- Argued, not tested: a power cut between the steps above. The tests put
  the same states on disk by hand; they do not cut power.

The earlier per-chunk `put` did not sync the directory after the rename, so a
crash could lose a chunk that a ledger event named. The durable path now
syncs it.
