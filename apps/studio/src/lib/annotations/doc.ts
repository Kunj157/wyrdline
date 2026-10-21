import * as A from "@automerge/automerge/slim";

/**
 * Annotation documents (plan §19 Phase 12): one Automerge document per
 * session holds the team's comments anchored to ledger `seq` ranges, the
 * triage state of the session's divergences and the pinned checkpoints.
 * Every entry lives under a map keyed by a stable id, so concurrent edits to
 * different entries merge and edits to the same field resolve the same way
 * on every replica.
 */

export const TRIAGE_STATES = ["open", "acknowledged", "false_positive"] as const;
export type TriageState = (typeof TRIAGE_STATES)[number];

export interface Comment {
  id: string;
  /** First and last ledger `seq` the comment is about (inclusive). */
  from: number;
  to: number;
  author: string;
  text: string;
  /** Unix milliseconds. */
  at: number;
  resolved: boolean;
}

export interface Triage {
  state: TriageState;
  note: string;
  by: string;
  at: number;
}

export interface Pin {
  label: string;
  by: string;
  at: number;
}

export interface Annotations {
  v: number;
  session: string;
  comments: Record<string, Comment>;
  triage: Record<string, Triage>;
  pins: Record<string, Pin>;
}

export type AnnotationDoc = A.Doc<Annotations>;

/** Bounds on what a replica accepts and writes (sync input is untrusted). */
export const LIMITS = {
  text: 4000,
  note: 1000,
  label: 200,
  author: 100,
  id: 128,
  comments: 5000,
} as const;

/**
 * The shared root every replica starts from. It is built deterministically
 * (fixed actor, no timestamp) so two replicas that start offline produce the
 * byte-identical first change instead of two competing root maps, one of
 * which would hide the other's entries after a merge.
 */
export function createAnnotations(session: string): AnnotationDoc {
  const base = A.change(A.init<Annotations>({ actor: "00" }), { time: undefined }, (d) => {
    d.v = 1;
    d.session = session;
    d.comments = {};
    d.triage = {};
    d.pins = {};
  });
  return A.clone(base);
}

export function loadAnnotations(bytes: Uint8Array): AnnotationDoc {
  return A.load<Annotations>(bytes);
}

const now = () => Date.now();

function bounded(what: string, s: string, max: number): string {
  const t = s.trim();
  if (!t) throw new Error(`${what} is empty`);
  if (t.length > max) throw new Error(`${what} is longer than ${max} characters`);
  return t;
}

function newId(): string {
  return crypto.randomUUID();
}

export interface NewComment {
  from: number;
  to: number;
  text: string;
  author: string;
}

export function addComment(doc: AnnotationDoc, c: NewComment, id = newId()): AnnotationDoc {
  if (!Number.isSafeInteger(c.from) || !Number.isSafeInteger(c.to) || c.from < 0 || c.to < c.from)
    throw new Error("a comment needs a seq range with 0 ≤ from ≤ to");
  if (Object.keys(doc.comments).length >= LIMITS.comments)
    throw new Error(`a session holds at most ${LIMITS.comments} comments`);
  const text = bounded("comment", c.text, LIMITS.text);
  const author = bounded("author", c.author, LIMITS.author);
  return A.change(doc, (d) => {
    d.comments[id] = { id, from: c.from, to: c.to, author, text, at: now(), resolved: false };
  });
}

export function editComment(doc: AnnotationDoc, id: string, text: string): AnnotationDoc {
  const t = bounded("comment", text, LIMITS.text);
  return A.change(doc, (d) => {
    const c = d.comments[id];
    if (c) c.text = t;
  });
}

export function resolveComment(doc: AnnotationDoc, id: string, resolved: boolean): AnnotationDoc {
  return A.change(doc, (d) => {
    const c = d.comments[id];
    if (c) c.resolved = resolved;
  });
}

export function deleteComment(doc: AnnotationDoc, id: string): AnnotationDoc {
  return A.change(doc, (d) => {
    Reflect.deleteProperty(d.comments, id);
  });
}

export function setTriage(
  doc: AnnotationDoc,
  divergence: string,
  state: TriageState,
  by: string,
  note = "",
): AnnotationDoc {
  const key = bounded("divergence id", divergence, LIMITS.id);
  const who = bounded("author", by, LIMITS.author);
  if (note.length > LIMITS.note) throw new Error(`note is longer than ${LIMITS.note} characters`);
  return A.change(doc, (d) => {
    d.triage[key] = { state, note: note.trim(), by: who, at: now() };
  });
}

export function pinCheckpoint(
  doc: AnnotationDoc,
  checkpoint: string,
  label: string,
  by: string,
): AnnotationDoc {
  const key = bounded("checkpoint id", checkpoint, LIMITS.id);
  const l = bounded("label", label, LIMITS.label);
  const who = bounded("author", by, LIMITS.author);
  return A.change(doc, (d) => {
    d.pins[key] = { label: l, by: who, at: now() };
  });
}

export function unpinCheckpoint(doc: AnnotationDoc, checkpoint: string): AnnotationDoc {
  return A.change(doc, (d) => {
    Reflect.deleteProperty(d.pins, checkpoint);
  });
}

// ---------------------------------------------------------------------------
// Reading: the document arrives from other replicas, so every field is
// checked and anything malformed is left out instead of rendered.

export interface AnnotationView {
  comments: Comment[];
  triage: Map<string, Triage>;
  pins: Map<string, Pin>;
}

const str = (v: unknown, max: number): string | null =>
  typeof v === "string" && v.length <= max ? v : null;
const int = (v: unknown): number | null =>
  typeof v === "number" && Number.isSafeInteger(v) && v >= 0 ? v : null;

function entries(v: unknown): [string, Record<string, unknown>][] {
  if (!v || typeof v !== "object") return [];
  return Object.entries(v as Record<string, unknown>).filter(
    (e): e is [string, Record<string, unknown>] =>
      e[0].length <= LIMITS.id && !!e[1] && typeof e[1] === "object",
  );
}

export function viewAnnotations(doc: AnnotationDoc): AnnotationView {
  const comments: Comment[] = [];
  for (const [id, c] of entries(doc.comments)) {
    const from = int(c.from);
    const to = int(c.to);
    const text = str(c.text?.toString(), LIMITS.text);
    const author = str(c.author?.toString(), LIMITS.author);
    const at = int(c.at);
    if (from === null || to === null || to < from || !text || !author || at === null) continue;
    comments.push({ id, from, to, text, author, at, resolved: c.resolved === true });
  }
  comments.sort((a, b) => a.from - b.from || a.at - b.at || a.id.localeCompare(b.id));
  const triage = new Map<string, Triage>();
  for (const [id, t] of entries(doc.triage)) {
    const state = TRIAGE_STATES.find((s) => s === t.state);
    const by = str(t.by?.toString(), LIMITS.author);
    const note = str(t.note?.toString() ?? "", LIMITS.note);
    const at = int(t.at);
    if (state && by && note !== null && at !== null) triage.set(id, { state, note, by, at });
  }
  const pins = new Map<string, Pin>();
  for (const [id, p] of entries(doc.pins)) {
    const label = str(p.label?.toString(), LIMITS.label);
    const by = str(p.by?.toString(), LIMITS.author);
    const at = int(p.at);
    if (label && by && at !== null) pins.set(id, { label, by, at });
  }
  return { comments, triage, pins };
}
