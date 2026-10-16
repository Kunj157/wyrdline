import type { Event } from "@wyrdline/schemas";
import { ApiError } from "@wyrdline/schemas/client";

import { createOfflineClient, parseExport, sessionFromEvents } from "./offline";

function ev(seq: number, body: Event["body"]): Event {
  return {
    seq,
    ts_mono_ns: seq * 1000,
    ts_wall_ms: 1_760_000_000_000 + seq,
    process: null,
    drops_since_last: 0,
    body,
  };
}

const events: Event[] = [
  ev(1, {
    kind: "meta.session.started",
    payload: {
      session_id: "01demo",
      agent_kind: "fakeagent",
      agent_version: null,
      command: ["agent"],
      workspace_root: "/w",
      mode: "cell",
      kernel_release: "6.17",
      capabilities: {},
      policy_set_hash: null,
    },
  }),
  ev(2, {
    kind: "meta.session.state_changed",
    payload: { from: "preparing", to: "recording", reason: null },
  }),
  ev(3, {
    kind: "state.checkpoint.created",
    payload: {
      checkpoint_id: "c1",
      branch_id: "a",
      parent_id: null,
      after_call_id: null,
      manifest: "b3:00",
      files_changed: 2,
      bytes_added: 10,
      duration_ms: 1,
    },
  }),
  ev(4, {
    kind: "state.checkpoint.forked",
    payload: { checkpoint_id: "c1", branch_id: "b", name: "try" },
  }),
];

describe("offline replay", () => {
  it("parses JSON lines and arrays, sorted by seq", () => {
    const lines = [...events]
      .reverse()
      .map((e) => JSON.stringify(e))
      .join("\n");
    expect(parseExport(lines).map((e) => e.seq)).toEqual([1, 2, 3, 4]);
    expect(parseExport(JSON.stringify(events))).toHaveLength(4);
    expect(() => parseExport("")).toThrow(/empty/);
    expect(() => parseExport('{"hello":1}')).toThrow(/not a Wyrdline event export/);
    expect(() => parseExport('{"seq":1,"body":{"kind":"x"}}\nnot json')).toThrow(/line 2/);
  });

  it("reconstructs the session, branches, and checkpoints", () => {
    const s = sessionFromEvents(events, "demo.jsonl");
    expect(s.id).toBe("01demo");
    expect(s.agent).toBe("fakeagent");
    expect(s.status).toBe("paused"); // a replay is never live
    expect(s.checkpoints.map((c) => c.id)).toEqual(["c1"]);
    expect(s.branches.map((b) => b.name)).toEqual(["a", "try"]);
    expect(s.current_branch).toBe("b");
  });

  it("serves reads, computes the graph once, and refuses mutations", async () => {
    let calls = 0;
    const client = createOfflineClient(events, "demo.jsonl", (json) => {
      calls += 1;
      expect(JSON.parse(json)).toHaveLength(4);
      return JSON.stringify({ calls: [], processes: [], effects: [], agent_root: null });
    });
    expect((await client.sessions()).sessions[0]?.id).toBe("01demo");
    expect(
      (await client.events("01demo", { after_seq: 2, limit: 1 })).events.map((e) => e.seq),
    ).toEqual([3]);
    await client.graph("01demo");
    await client.graph("01demo");
    expect(calls).toBe(1);
    await expect(client.rewind("01demo", "c1", { relaunch: false })).rejects.toBeInstanceOf(
      ApiError,
    );
    await expect(client.verify("01demo")).rejects.toThrow(/offline/);
    await expect(client.session("other")).rejects.toThrow(/not in this file/);
  });

  it("reports the graph as unavailable without the WebAssembly build", async () => {
    const client = createOfflineClient(events, "demo.jsonl", null);
    await expect(client.graph("01demo")).rejects.toThrow(/WebAssembly/);
  });
});
