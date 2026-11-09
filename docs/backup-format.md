# Backup format (`wyrdline.backup.v1`)

`wyrd backup <dir>` copies the data directory to `<dir>` as a consistent,
verified tree, and `wyrd restore <dir>` puts such a tree into an empty data
directory. This page is the contract for the tree: what is in it, what is left
out, and what a reader must check.

## What a backup is

A backup is a directory:

```
<dir>/
  BACKUP.json                        manifest, written last
  sessions/<id>/ledger.wyrdledger    one-file snapshot of the session ledger
  sessions/<id>/session.json         session record
  sessions/<id>/baseline.cbor        stat baseline (Cell sessions)
  sessions/<id>/import.json          provenance, imported sessions only
  sessions/<id>/launch.json          launch spec, when present
  sessions/<id>/judge-runs.jsonl     LLM judge run log, when present
  sessions/<id>/mcp-pins.json        MCP tool pins, when present
  cas/FORMAT, cas/chunks/...         content store (see cas-format.md)
  policies/                          uploaded Wasm components, when present
  keys/host.key                      only with --include-key (mode 0600)
```

The directory is mode 0700, and so is `keys/` when it exists. The manifest
is the only file that names the format:

```json
{
  "format": "wyrdline.backup.v1",
  "created_ms": 1767225600000,
  "wyrd_version": "1.0.0",
  "host_public_key": "<64 hex digits, or null>",
  "host_fingerprint": "<16 hex digits, or null>",
  "host_key_included": false,
  "sessions": [
    { "id": "...", "status": "closed", "events": 42,
      "head_hash": "b3:...", "sealed": true }
  ],
  "roots": 3,
  "chunks": 1287
}
```

`host_key_included` is true only when `keys/host.key` is in the backup.
`roots` is the number of checkpoint manifests the sessions reach, and
`chunks` the number of distinct chunks under them, all read back and checked.

## How a backup is taken

1. Refuse if the destination exists and is not an empty directory, or is
   inside the data directory (or contains it).
2. Take the store lock shared (`cas-format.md`, "Store lock") and hold it to
   the end. No collection can remove a chunk a copied record names.
3. Refuse if an import is in progress (a `sessions/.import-*` directory).
4. Refuse, naming them, if any session is running (its cgroup is populated)
   or its session lock is held. Nothing is copied yet.
5. For each session, under its own session lock: take the ledger snapshot
   (`VACUUM INTO`, one read transaction, so the copy is one file and holds
   every committed event, including those still in the write-ahead log), and
   copy the files listed above. A session that starts running or is locked
   meanwhile makes the backup fail.
6. Copy `policies/`, the host key if asked, and the content store. Each
   chunk is written once and never changes, so copying it under the shared
   lock is exact. The store is copied last: every chunk a copied record names
   was committed before that record was saved.
7. Sync the staging tree once (`syncfs`).
8. Verify the staging tree (see below). If anything fails, remove the staging
   tree and return the error. Nothing has been named yet.
9. Write `BACKUP.json` durably, then rename the staging directory to `<dir>`
   and sync its parent.

The staging directory is `.<name>.partial-<pid>`, beside `<dir>`. A directory
without `BACKUP.json` is never a complete backup. A refused backup (a running
session, an import in progress, an occupied destination) creates nothing.
`wyrd backup` exits 4 for a busy or occupied destination, and 1 for other
errors.

## What is left out, and why

- **`session.lock`, `control.token`, sockets, logs, relaunch files, the
  restored markers.** These are per-process or per-boot state, or secrets
  that belong to the running daemon.
- **`branches/*/work/`.** Overlay scratch space, which may be root-owned.
- **`branches/*/upper/`.** The overlay's upper layers hold the uncheckpointed
  edits of a branch, and may contain whiteout character devices that cannot be
  made without privileges. A backup therefore holds what was checkpointed,
  not the edits made since the last checkpoint. Checkpoint before backing up.
- **`keys/host.key`, unless `--include-key`.** The private seed signs seals
  and bundles as this host. Leave it out by default, and keep a backup that
  includes it as private as the key.
- **`cas/tmp/`, `cas/LOCK`.** Staging files of other writers and the lock.

## Restore

`wyrd restore <dir>` restores into the data directory (`$WYRD_DATA_DIR`, and
so on), which must be absent or empty; otherwise it exits 4 and touches
nothing. The backup is copied beside the data directory, the copy is verified
(below) and checked against the manifest's sessions, checkpoints and chunk
counts, and only then renamed into place. A backup with an entry other than
`BACKUP.json`, `sessions/`, `cas/`, `policies/` or `keys/` is refused.

The host private key is restored only if the backup has it, with mode 0600.
Without it, the next session closed in the restored directory is sealed with
a new key, and its fingerprint differs from the old one. Sessions sealed
before the backup still verify against the keys in their own seals.

A restored session keeps its ledger, record, checkpoints and baseline, so
`wyrd verify`, `wyrd checkpoints`, `wyrd diff` between checkpoints, and
applying a checkpoint all work. Branch overlays are not in a backup. A restore
rebuilds each branch's overlay from its head checkpoint (for a fork, the
checkpoint it was forked from), as a rewind does, so an open session
checkpoints on from its last checkpoint. The edits made after that checkpoint
are not restored. Rebuilding a checkpoint that records deleted files needs
device nodes; without the privilege for them the restore fails, names the
branch, and leaves the data directory untouched. Session
records store absolute paths (the workspace, the policy, the cgroup); they are
copied as they were, so a restore that needs the same workspace works where
that path exists.

## Verification

`verify_tree` checks a copy: every session record reads and names its own
directory, every ledger passes `wyrd verify` (the chain, and the seal's
signature when sealed), and every chunk reachable from a checkpoint reads back
with the hash its name gives. It deletes nothing and repairs nothing. A
backup runs it on the staging tree before the manifest is written, and a
restore runs it on its own copy before anything is renamed into place.

## Compatibility

Backups are not signed. The manifest's chunk and ledger checks prove that the
copy is internally consistent. They do not prove who made it. A backup that
includes the host key carries the same authority as the key.

A reader that meets a manifest with another `format` must refuse it.

