import { useCallback, useEffect, useState, useSyncExternalStore } from "react";

import { loadAutomerge } from "./automerge";
import type { AnnotationView } from "./doc";
import { AnnotationReplica } from "./replica";
import { type AnnotationStore, indexedDbStore, memoryStore } from "./store";

let store: AnnotationStore | undefined;
function defaultStore(): AnnotationStore {
  store ??= typeof indexedDB === "undefined" ? memoryStore() : indexedDbStore();
  return store;
}

/** The session's annotation replica, opened from the local store. */
export function useAnnotations(sid: string): {
  replica: AnnotationReplica | null;
  view: AnnotationView | null;
  error: string | null;
} {
  const [replica, setReplica] = useState<AnnotationReplica | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    let opened: AnnotationReplica | null = null;
    loadAutomerge()
      .then(() => AnnotationReplica.open(sid, defaultStore()))
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
  }, [sid]);
  const subscribe = useCallback(
    (l: () => void) => (replica ? replica.subscribe(l) : () => undefined),
    [replica],
  );
  const view = useSyncExternalStore(subscribe, () => replica?.view() ?? null);
  return { replica, view, error };
}
