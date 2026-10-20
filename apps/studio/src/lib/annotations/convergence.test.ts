import * as A from "@automerge/automerge/slim";
import fc from "fast-check";
import { beforeAll, describe, expect, it } from "vitest";

import {
  addComment,
  type AnnotationDoc,
  type Annotations,
  createAnnotations,
  deleteComment,
  editComment,
  pinCheckpoint,
  resolveComment,
  setTriage,
  TRIAGE_STATES,
  unpinCheckpoint,
  viewAnnotations,
} from "./doc";
import { SyncPeer } from "./sync";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

/**
 * Two clients, each linked to a hub replica (the topology Studio uses with
 * the Hub), exchange real Automerge sync messages over a simulated network
 * where messages queue, arrive in any interleaving with edits, and are lost
 * when a link drops. A client may stay offline for a while and keep editing.
 * Whatever the schedule, once everyone reconnects and the links go quiet,
 * all replicas must hold the same document.
 */

const CLIENTS = 2;
const DIVERGENCES = ["dv1", "dv2"];
const CHECKPOINTS = ["cp1", "cp2", "cp3"];

type Edit =
  | { t: "add"; from: number; len: number; text: string }
  | { t: "edit"; pick: number; text: string }
  | { t: "resolve"; pick: number; resolved: boolean }
  | { t: "delete"; pick: number }
  | { t: "triage"; div: number; state: number; note: string }
  | { t: "pin"; cp: number; label: string }
  | { t: "unpin"; cp: number };

type Step =
  | { k: "edit"; client: number; edit: Edit }
  | { k: "send"; client: number; up: boolean }
  | { k: "deliver"; client: number; up: boolean }
  | { k: "drop"; client: number }
  | { k: "offline"; client: number; offline: boolean }
  | { k: "settle" };

const text = fc.string({ minLength: 1, maxLength: 12 }).filter((s) => s.trim().length > 0);
const client = fc.nat({ max: CLIENTS - 1 });
const edit: fc.Arbitrary<Edit> = fc.oneof(
  fc.record({ t: fc.constant("add" as const), from: fc.nat(50), len: fc.nat(5), text }),
  fc.record({ t: fc.constant("edit" as const), pick: fc.nat(), text }),
  fc.record({ t: fc.constant("resolve" as const), pick: fc.nat(), resolved: fc.boolean() }),
  fc.record({ t: fc.constant("delete" as const), pick: fc.nat() }),
  fc.record({
    t: fc.constant("triage" as const),
    div: fc.nat(DIVERGENCES.length - 1),
    state: fc.nat(TRIAGE_STATES.length - 1),
    note: fc.string({ maxLength: 8 }),
  }),
  fc.record({ t: fc.constant("pin" as const), cp: fc.nat(CHECKPOINTS.length - 1), label: text }),
  fc.record({ t: fc.constant("unpin" as const), cp: fc.nat(CHECKPOINTS.length - 1) }),
);
const step: fc.Arbitrary<Step> = fc.oneof(
  { weight: 4, arbitrary: fc.record({ k: fc.constant("edit" as const), client, edit }) },
  {
    weight: 4,
    arbitrary: fc.record({ k: fc.constant("send" as const), client, up: fc.boolean() }),
  },
  {
    weight: 4,
    arbitrary: fc.record({ k: fc.constant("deliver" as const), client, up: fc.boolean() }),
  },
  { weight: 1, arbitrary: fc.record({ k: fc.constant("drop" as const), client }) },
  {
    weight: 1,
    arbitrary: fc.record({ k: fc.constant("offline" as const), client, offline: fc.boolean() }),
  },
  { weight: 1, arbitrary: fc.constant({ k: "settle" as const }) },
);

let ids = 0;
/** Comment ids ever added and ever deleted, across all clients. */
const added = new Set<string>();
const deleted = new Set<string>();

function apply(doc: AnnotationDoc, e: Edit, who: string): AnnotationDoc {
  const comments = viewAnnotations(doc).comments;
  const pick = (n: number) => comments[n % Math.max(comments.length, 1)]?.id;
  switch (e.t) {
    case "add": {
      const id = `c${ids++}`;
      added.add(id);
      return addComment(doc, { from: e.from, to: e.from + e.len, text: e.text, author: who }, id);
    }
    case "edit": {
      const id = pick(e.pick);
      return id ? editComment(doc, id, e.text) : doc;
    }
    case "resolve": {
      const id = pick(e.pick);
      return id ? resolveComment(doc, id, e.resolved) : doc;
    }
    case "delete": {
      const id = pick(e.pick);
      if (id) deleted.add(id);
      return id ? deleteComment(doc, id) : doc;
    }
    case "triage":
      return setTriage(
        doc,
        DIVERGENCES[e.div] ?? "dv1",
        TRIAGE_STATES[e.state] ?? "open",
        who,
        e.note,
      );
    case "pin":
      return pinCheckpoint(doc, CHECKPOINTS[e.cp] ?? "cp1", e.label, who);
    case "unpin":
      return unpinCheckpoint(doc, CHECKPOINTS[e.cp] ?? "cp1");
  }
}

/** A client ↔ hub link: one peer object per side and a queue per direction. */
interface Link {
  client: SyncPeer<Annotations>;
  hub: SyncPeer<Annotations>;
  up: Uint8Array[];
  down: Uint8Array[];
  offline: boolean;
}

function run(steps: Step[]) {
  added.clear();
  deleted.clear();
  const hubDoc = { doc: createAnnotations("s") };
  const links: Link[] = Array.from({ length: CLIENTS }, () => ({
    client: new SyncPeer(createAnnotations("s")),
    hub: new SyncPeer(hubDoc.doc),
    up: [],
    down: [],
    offline: false,
  }));
  // The hub keeps one document; each link's hub-side peer reads and writes it.
  const hubSide = (l: Link, f: (p: SyncPeer<Annotations>) => void) => {
    l.hub.doc = hubDoc.doc;
    f(l.hub);
    hubDoc.doc = l.hub.doc;
  };
  const drop = (l: Link) => {
    l.up.length = 0;
    l.down.length = 0;
    l.client.reset();
    l.hub.reset();
  };
  const send = (l: Link, up: boolean) => {
    if (l.offline) return;
    if (up) {
      const m = l.client.outgoing();
      if (m) l.up.push(m);
    } else {
      hubSide(l, (p) => {
        const m = p.outgoing();
        if (m) l.down.push(m);
      });
    }
  };
  const deliver = (l: Link, up: boolean) => {
    if (l.offline) return false;
    const m = (up ? l.up : l.down).shift();
    if (!m) return false;
    if (up) hubSide(l, (p) => p.receive(m));
    else l.client.receive(m);
    return true;
  };

  // Pump every online link until none has anything left to say.
  const settle = () => {
    for (let round = 0; round < 200; round++) {
      let busy = false;
      for (const l of links) {
        send(l, true);
        send(l, false);
        while (deliver(l, true) || deliver(l, false)) busy = true;
      }
      if (!busy) return;
    }
    throw new Error("sync did not settle");
  };

  for (const s of steps) {
    if (s.k === "settle") {
      settle();
      continue;
    }
    const l = links[s.client];
    if (!l) continue;
    switch (s.k) {
      case "edit":
        l.client.doc = apply(l.client.doc, s.edit, `user${s.client}`);
        break;
      case "send":
        send(l, s.up);
        break;
      case "deliver":
        deliver(l, s.up);
        break;
      case "drop":
        drop(l);
        break;
      case "offline":
        // Going offline loses whatever was in flight, like a dropped socket.
        if (s.offline && !l.offline) drop(l);
        l.offline = s.offline;
        break;
    }
  }

  // Everyone reconnects.
  for (const l of links) {
    if (l.offline) drop(l);
    l.offline = false;
  }
  settle();
  return { hub: hubDoc.doc, clients: links.map((l) => l.client.doc) };
}

const sortedHeads = (d: AnnotationDoc) => [...A.getHeads(d)].sort();

describe("annotation sync convergence", () => {
  it("converges for any interleaving of edits, delivery, drops and offline periods", () => {
    fc.assert(
      fc.property(fc.array(step, { maxLength: 80 }), (steps) => {
        const { hub, clients } = run(steps);
        // No edit is lost: exactly the comments nobody deleted survive.
        const kept = [...added].filter((id) => !deleted.has(id)).sort();
        expect(
          viewAnnotations(hub)
            .comments.map((c) => c.id)
            .sort(),
        ).toEqual(kept);
        for (const c of clients) {
          expect(sortedHeads(c)).toEqual(sortedHeads(hub));
          expect(A.toJS(c)).toEqual(A.toJS(hub));
          expect(viewAnnotations(c)).toEqual(viewAnnotations(hub));
        }
      }),
      { numRuns: 150 },
    );
  });

  it("merges offline edits from both sides of the same comment", () => {
    const steps: Step[] = [
      { k: "edit", client: 0, edit: { t: "add", from: 4, len: 2, text: "shared" } },
      { k: "settle" },
      { k: "offline", client: 0, offline: true },
      { k: "edit", client: 0, edit: { t: "resolve", pick: 0, resolved: true } },
      { k: "edit", client: 0, edit: { t: "pin", cp: 0, label: "offline pin" } },
      { k: "edit", client: 1, edit: { t: "edit", pick: 0, text: "edited online" } },
      { k: "edit", client: 1, edit: { t: "triage", div: 0, state: 1, note: "seen" } },
    ];
    const { hub, clients } = run(steps);
    const v = viewAnnotations(hub);
    expect(v.comments).toHaveLength(1);
    expect(v.comments[0]).toMatchObject({ text: "edited online", resolved: true });
    expect(v.pins.get("cp1")?.label).toBe("offline pin");
    expect(v.triage.get("dv1")?.state).toBe("acknowledged");
    for (const c of clients) expect(viewAnnotations(c)).toEqual(v);
  });
});
