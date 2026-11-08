import type { Event } from "@wyrdline/schemas";
import { describe as suite, expect, it } from "vitest";

import {
  describe,
  irreversibleAfter,
  isIrreversible,
  layoutRiver,
  meaningOf,
  transcript,
} from "./model";

let seq = 0;
function ev(body: Event["body"]): Event {
  seq += 1;
  return { seq, ts_mono_ns: seq * 1000, ts_wall_ms: seq, process: null, drops_since_last: 0, body };
}
const file = (path: string, in_cell = true) => ({ path, in_cell, truncated: false });

suite("model", () => {
  const events: Event[] = [
    ev({ kind: "intent.message", payload: { role: "user", turn: 0, text: "fix the cart test" } }),
    ev({
      kind: "intent.tool_call.started",
      payload: {
        call_id: "c1",
        source: "hook",
        tool: "Bash",
        args: { command: "pnpm test" },
        turn: 1,
      },
    }),
    ev({ kind: "effect.file.write", payload: { file: file("src/cart.ts"), bytes: 10 } }),
    ev({
      kind: "effect.file.open_write",
      payload: { file: file("/home/u/.bashrc", false), flags: 1 },
    }),
    ev({
      kind: "effect.net.connect",
      payload: {
        family: 2,
        addr: "1.2.3.4",
        port: 443,
        dns_name: "evil.example",
        verdict: "denied",
      },
    }),
    ev({
      kind: "state.checkpoint.created",
      payload: {
        checkpoint_id: "c1",
        branch_id: "a",
        parent_id: null,
        after_call_id: "c1",
        manifest: "b3:x",
        files_changed: 1,
        bytes_added: 10,
        duration_ms: 3,
      },
    }),
    ev({
      kind: "effect.net.connect",
      payload: { family: 2, addr: "5.6.7.8", port: 443, dns_name: null, verdict: "allowed" },
    }),
    ev({
      kind: "intent.message",
      payload: { role: "assistant", turn: 1, text: "All tests pass." },
    }),
  ];

  const at = (i: number): Event => {
    const e = events[i];
    if (!e) throw new Error(`no event ${i}`);
    return e;
  };

  it("describes events for people", () => {
    expect(describe(at(1))).toBe("Bash pnpm test");
    expect(describe(at(4))).toBe("connect evil.example:443 (denied)");
    expect(describe(at(5))).toBe("checkpoint c1 (1 file)");
    expect(
      describe(
        ev({
          kind: "meta.rewind.requested",
          payload: { checkpoint_id: "c1", reason: "broke it", source: "mcp" },
        }),
      ),
    ).toBe("agent asks to rewind to c1");
    expect(
      describe(
        ev({
          kind: "meta.rewind.rejected",
          payload: { request_seq: 9, by: "studio", note: null },
        }),
      ),
    ).toBe("rewind request 9 rejected (studio)");
  });

  it("flags what a rewind cannot undo", () => {
    expect(isIrreversible(at(2))).toBe(false);
    expect(isIrreversible(at(3))).toBe(true);
    expect(isIrreversible(at(4))).toBe(false);
    expect(isIrreversible(at(6))).toBe(true);
    expect(meaningOf(at(4))).toBe("denied");
    expect(meaningOf(at(3))).toBe("irreversible");
    expect(irreversibleAfter(events, { seq: 6, branch: "a" }).map((e) => e.seq)).toEqual([7]);
  });

  it("extracts the transcript", () => {
    expect(transcript(events).map((t) => t.text)).toEqual(["fix the cart test", "All tests pass."]);
  });

  it("lays the river out with threads from calls to their effects", () => {
    const graph = {
      calls: [
        {
          call_id: "c1",
          tool: "Bash",
          source: "hook" as const,
          started_seq: 2,
          finished_seq: null,
          start_ns: 0,
          end_ns: 0,
          status: null,
          exit_code: null,
          command: "pnpm test",
          target_path: null,
          nested_in: null,
          effects: [3],
        },
      ],
      processes: [],
      effects: [
        {
          seq: 3,
          kind: "effect.file.write",
          ts_ns: 0,
          pid: 1,
          call_id: "c1",
          candidates: [],
          attribution: "direct" as const,
          confidence_bp: 10000,
        },
        {
          seq: 7,
          kind: "effect.net.connect",
          ts_ns: 0,
          pid: 1,
          call_id: null,
          candidates: ["c1"],
          attribution: "ambiguous" as const,
          confidence_bp: 5000,
        },
      ],
      agent_root: null,
    };
    const r = layoutRiver(events, graph);
    expect(r.knots.map((k) => k.lane)).toEqual([
      "intent",
      "effect",
      "effect",
      "effect",
      "spine",
      "effect",
    ]);
    expect(r.threads).toEqual([
      { from: 2, to: 3, callId: "c1", ambiguous: false },
      { from: 2, to: 7, callId: "c1", ambiguous: true },
    ]);
    const xs = r.knots.map((k) => k.x);
    expect([...xs].sort((a, b) => a - b)).toEqual(xs);
  });

  it("draws divergence evidence as divergence knots, even off the usual lanes", () => {
    const r = layoutRiver(events, undefined, new Set([4, 8]));
    const bySeq = new Map(r.knots.map((k) => [k.seq, k]));
    expect(bySeq.get(4)).toMatchObject({ lane: "effect", meaning: "divergence" });
    // The assistant's claim is not a tool call, but it is the evidence.
    expect(bySeq.get(8)).toMatchObject({ lane: "intent", meaning: "divergence" });
    expect(bySeq.get(5)?.meaning).toBe("denied");
    expect(bySeq.has(1)).toBe(false);
  });

  suite("what a rewind or fork leaves behind (the CLI's rule)", () => {
    // Seqs: 1 c1 on a, 2 egress on a, 3 rewind to c1 -> b, 4 c2 on b,
    // 5 egress on b, 6 fork from c2 -> c, 7 egress on c.
    const s = (n: number, body: Event["body"]): Event => ({
      seq: n,
      ts_mono_ns: n,
      ts_wall_ms: n,
      process: null,
      drops_since_last: 0,
      body,
    });
    const egress = (n: number) =>
      s(n, {
        kind: "effect.net.connect",
        payload: { family: 2, addr: `10.0.0.${n}`, port: 443, dns_name: null, verdict: "allowed" },
      });
    const log: Event[] = [
      s(1, { kind: "intent.message", payload: { role: "user", turn: 0, text: "go" } }),
      egress(2),
      s(3, {
        kind: "state.checkpoint.rewound",
        payload: { checkpoint_id: "c1", branch_id: "b", irreversible_seqs: [2] },
      }),
      s(4, { kind: "intent.message", payload: { role: "user", turn: 1, text: "again" } }),
      egress(5),
      s(6, {
        kind: "state.checkpoint.forked",
        payload: { checkpoint_id: "c2", branch_id: "c", name: "exp" },
      }),
      egress(7),
    ];
    const cp = (id: string, seq: number, branch: string) => ({
      id,
      branch,
      parent: null,
      manifest: "b3:00",
      seq,
      created_ms: 0,
      files: 0,
      bytes_read: 0,
    });
    const branch = (id: string, forked_from: string | null) => ({
      id,
      name: id,
      forked_from,
      created_ms: 0,
    });
    const tree = (current: string) => ({
      current_branch: current,
      branches: [branch("a", null), branch("b", "c1"), branch("c", "c2")],
      checkpoints: [cp("c1", 1, "a"), cp("c2", 4, "b")],
    });
    const seqs = (es: Event[]) => es.map((e) => e.seq);
    // While b is current, the fork to c has not happened yet.
    const onB = log.slice(0, 5);
    const treeB = () => ({ ...tree("b"), branches: tree("b").branches.slice(0, 2) });

    it("lists the current branch's effects after the checkpoint", () => {
      expect(seqs(irreversibleAfter(onB, { seq: 4, branch: "b" }, treeB()))).toEqual([5]);
    });

    it("does not repeat effects on a branch already left", () => {
      // On b, back to c1: egress 2 was on a, which the rewind to c1 left.
      expect(seqs(irreversibleAfter(onB, { seq: 1, branch: "a" }, treeB()))).toEqual([5]);
    });

    it("walks back across a fork to the checkpoint's branch", () => {
      // On c (forked from c2 on b), back to c1: c's egress 7, and b's
      // between c1 and c2 (none). Egress 5 came after c2 on b; the fork
      // that left b already listed it.
      expect(seqs(irreversibleAfter(log, { seq: 1, branch: "a" }, tree("c")))).toEqual([7]);
    });

    it("from the branch's own fork point, lists only that branch", () => {
      expect(seqs(irreversibleAfter(log, { seq: 4, branch: "b" }, tree("c")))).toEqual([7]);
    });

    it("without the branch tree, lists every effect after the checkpoint", () => {
      expect(seqs(irreversibleAfter(log, { seq: 1, branch: "a" }))).toEqual([2, 5, 7]);
    });
  });
});
