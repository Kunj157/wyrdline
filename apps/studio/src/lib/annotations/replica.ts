import * as A from "@automerge/automerge/slim";

import { type AnnotationDoc, type AnnotationView, createAnnotations, viewAnnotations } from "./doc";
import type { AnnotationStore } from "./store";

export interface AnnotationSnapshot {
  view: AnnotationView;
  /** Every change is in the local store. */
  saved: boolean;
}

/** How long local edits are batched before they are written to the store. */
const SAVE_DELAY_MS = 150;

/**
 * The annotation document of one session on this device: loaded from the
 * store (or created), changed by local edits and by the sync client, saved
 * back after every change, and observed by the UI.
 */
export class AnnotationReplica {
  private listeners = new Set<() => void>();
  private saving: ReturnType<typeof setTimeout> | null = null;
  private cached: AnnotationSnapshot | null = null;
  private dirty = false;
  /** Called after local edits only (the sync client sends them). */
  private localListeners = new Set<() => void>();

  private constructor(
    readonly session: string,
    private readonly key: string,
    private current: AnnotationDoc,
    private store: AnnotationStore,
  ) {}

  /**
   * Open the replica of `session`, stored under `key` (default: the session
   * id). A page synced with a Hub bundle keys by the bundle, so annotations
   * made against another bundle, or on this device alone, never reach it.
   */
  static async open(
    session: string,
    store: AnnotationStore,
    key: string = session,
  ): Promise<AnnotationReplica> {
    const saved = await store.load(key).catch(() => null);
    // The root change is shared by every replica, so a fresh document
    // merges with what other replicas already hold.
    let doc = createAnnotations(session);
    if (saved) {
      try {
        doc = A.merge(doc, A.load(saved));
      } catch {
        // A corrupt local copy is dropped; the Hub has the shared state.
      }
    }
    return new AnnotationReplica(session, key, doc, store);
  }

  get doc(): AnnotationDoc {
    return this.current;
  }

  view(): AnnotationView {
    return this.snapshot().view;
  }

  /** What the UI renders; the same object until something changes. */
  snapshot(): AnnotationSnapshot {
    this.cached ??= { view: viewAnnotations(this.current), saved: !this.dirty };
    return this.cached;
  }

  /** Apply a local edit (one of the `doc.ts` operations). */
  change(edit: (d: AnnotationDoc) => AnnotationDoc): void {
    this.replace(edit(this.current));
    for (const l of this.localListeners) l();
  }

  /** Take a document that already contains this one's changes (sync). */
  replace(doc: AnnotationDoc): void {
    if (doc === this.current) return;
    this.current = doc;
    this.dirty = true;
    this.scheduleSave();
    this.notify();
  }

  /** Run `listener` after every local edit (not after `replace`); returns the unsubscribe. */
  onLocalChange(listener: () => void): () => void {
    this.localListeners.add(listener);
    return () => this.localListeners.delete(listener);
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Write pending changes now (tests, page hide). */
  async flush(): Promise<void> {
    if (this.saving) clearTimeout(this.saving);
    this.saving = null;
    await this.save();
  }

  private scheduleSave() {
    if (this.saving) return;
    this.saving = setTimeout(() => {
      this.saving = null;
      void this.save().catch(() => undefined);
    }, SAVE_DELAY_MS);
  }

  private async save() {
    const doc = this.current;
    await this.store.save(this.key, A.save(doc));
    // Still saved only if nothing changed while the write was in flight.
    if (doc === this.current && this.dirty) {
      this.dirty = false;
      this.notify();
    }
  }

  private notify() {
    this.cached = null;
    for (const l of this.listeners) l();
  }
}
