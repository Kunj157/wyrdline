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

## Garbage collection

Mark from root manifest hashes, sweep every unreferenced chunk. `dry_run`
reports without deleting.

## Durability

`put`, `put_stream`, `put_manifest` and `snapshot` are durable: when they
return, the chunk, its directory entry and the write are on disk (the file
is synced, renamed, and the chunk directory is synced).

A checkpoint uses the `_unsynced` variants inside the freeze and then one
`Cas::sync()`, which is a `syncfs` on the store's filesystem:

1. inside the freeze, changed files are chunked and written to `tmp/`, then
   renamed into `chunks/`, with no sync;
2. the cgroup is thawed;
3. the manifest is written and renamed, with no sync;
4. `Cas::sync()` (the barrier);
5. only then is the `CheckpointCreated` event appended to the ledger.

So the ledger never names a chunk or manifest that is not on disk: a crash
before step 5 leaves at most unreferenced chunks, which `gc` removes. The
crash ordering is argued from the steps above; a unit test checks the
read-back of an unsynced batch after `sync`, not a crash.

An unsynced chunk may already exist when a later durable `put` finds it, so
a durable `put` of an existing chunk also calls `sync()` before returning.

The earlier per-chunk `put` did not sync the directory after the rename, so
a crash could lose a chunk the ledger named. The durable path now does.

