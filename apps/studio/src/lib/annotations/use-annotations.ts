import { useCallback, useEffect, useState, useSyncExternalStore } from "react";

import { loadAutomerge } from "./automerge";
import { SyncClient, type StatusReport, syncPath } from "./client";
import { type Annotations, createAnnotations } from "./doc";
import { AnnotationReplica, type AnnotationSnapshot } from "./replica";
import { type AnnotationStore, indexedDbStore, memoryStore } from "./store";

let store: AnnotationStore | undefined;
function defaultStore(): AnnotationStore {
  store ??= typeof indexedDB === "undefined" ? memoryStore() : indexedDbStore();
  return store;
}

/**
 * The Hub bundle this page's annotations sync with, from `?hub=<id>`
 * (`?bundle=` is taken: it opens a bundle file in bundle mode).
 * Null when the page is not Hub-backed (annotations stay on this device).
 */
export function bundleFromLocation(search = globalThis.location?.search ?? ""): string | null {
  const id = new URLSearchParams(search).get("hub");
  return id && /^[A-Za-z0-9_-]{1,64}$/.test(id) ? id : null;
}

/**
 * The session's annotation replica, opened from the local store and, when
 * `bundle` is given, kept in sync with the Hub's copy for that bundle.
 *
 * Local edits go into the replica first (and to the store); the sync client
 * takes them from there. Remote changes replace the replica's document.
 */
export function useAnnotations(
  sid: string,
  bundle: string | null = null,
): {
  replica: AnnotationReplica | null;
  snapshot: AnnotationSnapshot | null;
  error: string | null;
  /** Sync state, or null when the page is not synced with the Hub. */
  sync: StatusReport | null;
} {
  const [replica, setReplica] = useState<AnnotationReplica | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sync, setSync] = useState<StatusReport | null>(null);
  useEffect(() => {
    let live = true;
    let opened: AnnotationReplica | null = null;
    // A bundle has its own local copy: annotations made against another
    // bundle, or on this device alone, never reach its document.
    const key = bundle ? `bundle:${bundle}` : sid;
    loadAutomerge()
      .then(() => AnnotationReplica.open(sid, defaultStore(), key))
      .then((r) => {
        opened = r;
        if (live) setReplica(r);
      })
      .catch((e: unknown) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
      setReplica(null);
      void opened?.flush().catch(() => undefined);
    };
  }, [sid, bundle]);

  useEffect(() => {
    if (!replica || !bundle) return;
    const client = new SyncClient<Annotations>({
      url: syncPath(bundle),
      doc: replica.doc,
      // What the Hub is known to hold before it has said anything: the
      // shared root. A refusal rolls back to it, never to local edits.
      base: createAnnotations(sid),
      // Remote changes, and the confirmed state after a refusal.
      onChange: (doc) => replica.replace(doc),
      onStatus: setSync,
    });
    // Local edits are already in the replica; the client sends them.
    const unsubscribe = replica.onLocalChange(() => client.change(() => replica.doc));
    return () => {
      unsubscribe();
      client.close();
      setSync(null);
    };
  }, [replica, bundle, sid]);

  const subscribe = useCallback(
    (l: () => void) => (replica ? replica.subscribe(l) : () => undefined),
    [replica],
  );
  const snapshot = useSyncExternalStore(subscribe, () => replica?.snapshot() ?? null);
  return { replica, snapshot, error, sync };
}
