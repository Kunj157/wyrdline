/**
 * Local persistence of annotation documents: the saved Automerge bytes of
 * each session's document in IndexedDB, so annotations survive reloads and
 * offline periods until they reach the Hub.
 */
export interface AnnotationStore {
  load(session: string): Promise<Uint8Array | null>;
  save(session: string, bytes: Uint8Array): Promise<void>;
}

const DB = "wyrdline-annotations";
const STORE = "docs";

function request<T>(r: IDBRequest<T>): Promise<T> {
  return new Promise((ok, fail) => {
    r.onsuccess = () => ok(r.result);
    r.onerror = () => fail(r.error ?? new Error("IndexedDB request failed"));
  });
}

export function indexedDbStore(factory: IDBFactory = indexedDB): AnnotationStore {
  let db: Promise<IDBDatabase> | undefined;
  const open = () => {
    db ??= new Promise((ok, fail) => {
      const r = factory.open(DB, 1);
      r.onupgradeneeded = () => r.result.createObjectStore(STORE);
      r.onsuccess = () => ok(r.result);
      r.onerror = () => {
        db = undefined;
        fail(r.error ?? new Error("IndexedDB unavailable"));
      };
    });
    return db;
  };
  return {
    async load(session) {
      const tx = (await open()).transaction(STORE, "readonly");
      const v: unknown = await request(tx.objectStore(STORE).get(session));
      // Checked structurally: a value cloned by IndexedDB may come from
      // another realm, where `instanceof Uint8Array` is false.
      return ArrayBuffer.isView(v) ? new Uint8Array(v.buffer, v.byteOffset, v.byteLength) : null;
    },
    async save(session, bytes) {
      const tx = (await open()).transaction(STORE, "readwrite");
      await request(tx.objectStore(STORE).put(bytes, session));
    },
  };
}

/** In-memory store for private windows where IndexedDB is unavailable. */
export function memoryStore(): AnnotationStore {
  const m = new Map<string, Uint8Array>();
  return {
    load: async (s) => m.get(s) ?? null,
    save: async (s, b) => {
      m.set(s, b);
    },
  };
}
