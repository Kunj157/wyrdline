# Content-addressed store — `wyrdline.cas.v1`

Used for Cell checkpoints and, later, bundle content.

## Layout

```
<root>/
├── FORMAT                 # "wyrdline.cas.v1"
├── chunks/ab/cd/<hex>     # zstd(level 3) of the chunk bytes; <hex> = BLAKE3 of the *uncompressed* bytes
└── tmp/                   # staging for atomic writes (write → sync → rename → sync the directory)
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

## Durability of single writes

`put` writes one chunk, and `put_stream`, `put_manifest`, `snapshot` and
bundle import all write through it. Each new chunk does these steps, in
order:

1. Create any missing fan-out directories (`chunks/ab`, `chunks/ab/cd`). After
   each new directory is made, its parent is synced, before anything is
   written under it. A directory that another process made first is synced
   the same way, because that process may not have synced it.
2. Write the compressed chunk to `tmp/` and sync its data.
3. Rename it to its chunk name.
4. Sync the chunk's directory, so the new name is on disk.

Opening a store syncs the parent of each directory it creates
(`chunks/` and `tmp/`) in the same way.

The tests in `crates/wyrd-cas/tests/durable_order.rs` record these steps and
check the order: every created directory is synced in its parent before a
file is renamed into it, and every rename is followed by a sync of its
directory. A power cut is not simulated; the order is checked, not the disk.

