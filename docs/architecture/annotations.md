# Collaborative annotations

Teammates annotate a session together, live or offline, and their edits
merge (plan §2 step 12). Annotations are an [Automerge](https://automerge.org)
document per session, kept in the browser and synced with the Hub, which
keeps one document per bundle (`GET /api/v1/sync?bundle=…`, see
[the Hub](hub.md)).

## The document

One document per session, with three maps keyed by stable ids, so
concurrent edits to different entries merge and edits to the same field
resolve the same way everywhere:

| Map | Entry | Notes |
|---|---|---|
| `comments` | `{ id, from, to, author, text, at, resolved }` | anchored to the inclusive ledger `seq` range `from..=to` |
| `triage` | `{ state, note, by, at }` by divergence id | `open` / `acknowledged` / `false_positive`; the team's view, separate from the triage written to the ledger |
| `pins` | `{ label, by, at }` by checkpoint id | pinned checkpoints |

Every replica starts from the same root change (fixed actor, no timestamp)
for the session. Without it, two replicas created offline would each create
their own root maps and a merge would hide one side's entries; the
convergence property test fails if the root is not shared.

Documents come from other replicas, so Studio validates every field when it
reads one and drops malformed entries. Writes are bounded: comment text
4000 characters, triage note 1000, pin label 200, 5000 comments per session.
The source is `apps/studio/src/lib/annotations/`.

## Storage in Studio

- The replica is saved to IndexedDB (`wyrdline-annotations`) after every
  change, so annotations survive reloads and offline periods. An in-memory
  store is used where IndexedDB is unavailable.
- The Automerge WebAssembly module loads only when the Annotations tab
  opens.

## Sync with the Hub

The sync client (`apps/studio/src/lib/annotations/client.ts`) is attached to
the replica only when the page names a bundle: `?hub=<bundle id>`
(`?bundle=` opens a bundle file in bundle mode, so it is not reused). Without
it, annotations stay on this device.

- The local copy is keyed by the bundle (`bundle:<id>`), so the same session
  opened against two bundles keeps two separate documents, and a page without
  a bundle keeps its own.
- The socket is `wss://<hub>/api/v1/sync?bundle=<id>` on the page's own
  origin. The Hub authenticates the upgrade with the session cookie and
  refuses a cross-site Origin, so Studio must be served from the Hub's origin
  (a reverse proxy in front of both). A bearer token cannot be sent on a
  browser WebSocket.
- The server sends one text frame first, `{"role":"viewer"|"member"|"admin"}`.
  A viewer's panel is read-only.
- Local edits are applied to the replica at once and sent when the socket is
  open. While it is not, they wait in the document, and the sync protocol
  sends them after reconnecting, so nothing is lost.
- Reconnects use exponential backoff (0.5 s to 10 s, jittered), and start at
  once when the browser comes back online.
- A change the Hub refuses comes back as a text frame
  `{"error":…,"message":…}` followed by a close with code 1008. The client
  rolls the document back to the last state the server confirmed (at first,
  the shared root), and the panel says why. Edits that were never refused are
  kept.
- A `read_only` refusal ends the connection for good. A viewer's first
  connection to a bundle with no shared document yet sends the root change,
  which the Hub refuses as a write; retrying would repeat that forever. The
  viewer sees the shared state once a member has written, when the page is
  next opened.

## Hub side

`GET /api/v1/sync?bundle=…` ([hub.md](hub.md#annotation-sync-endpoint-159)):
the bundle must belong to an organisation the caller is in, else 404.
Viewers follow the document and cannot change it; members and admins write.
The relay (`crates/wyrd-sync`) keeps one room per open document and persists
it per bundle. `wyrd_hub_sync_*` metrics count connections, messages,
clients, documents and document bytes.

## Bundles and export

The annotations panel has a **Download annotations** action. It saves the
replica's document as `<session>.annotations.automerge`, the file
`wyrd export --annotations <file>` takes. Export then writes it into a
`standard` bundle as `annotations.automerge`. A `strict` bundle never
includes it. See [the bundle format](../bundle-format.md#annotationsautomerge).

The `wyrd export` flag replaced the wyrdd directory read from the original
commit (`a24dc5e`). That commit does not compile on its own, and `ca2150b`
is the fix.

## Tests

- `apps/studio/src/lib/annotations/convergence.test.ts`: two clients and a
  hub replica over a simulated network, with random interleavings of edits,
  delivery, dropped links and offline periods (fast-check). All replicas
  converge, and exactly the comments nobody deleted survive.
- `apps/studio/src/lib/annotations/client.test.ts`: reconnects, role frames,
  refused changes rolled back, edits kept across a dropped socket.
- `apps/studio/src/lib/annotations/replica.test.ts`, `store.test.ts`:
  persistence and corrupt local copies.
- `crates/wyrd-sync/tests/relay.rs` and `crates/wyrd-hub/tests/containers_sync.rs`:
  the relay and the Hub endpoint (roles, cross-organisation access, limits,
  persistence).
- `crates/wyrd-cli/tests/bundle.rs`: annotations are included in standard
  bundles only.
- `apps/studio/src/lib/annotations/export.test.ts`: the downloaded file loads
  back with its comments.
- `tests/e2e/hub/annotations-hub.spec.ts` (`make test-e2e-hub`, CI only):
  three browser contexts on one bundle. Two members sync live, one goes
  offline and edits, and the edits merge; a viewer sees the panel read-only
  and no form; a fresh browser reads the merged document from the Hub; the
  download action saves the file.

The browser test runs Studio, the session API and the Hub behind one
same-origin proxy (`tests/e2e/hub/global-setup.ts`). The session data comes
from the wyrdd fixture, because the Studio session page still reads it from
`wyrdd`. The annotation sync uses the Hub. The teammates' sessions and the
bundle row are seeded with psql, since the Hub's upload endpoint is #198 and
the sync endpoint reads only the bundle row.
