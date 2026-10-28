import type { Event } from "@wyrdline/schemas";
import { describe, expect, it } from "vitest";

import {
  type BundleReport,
  type BundleView,
  BundleError,
  createBundleClient,
  isBundle,
  openSessionFile,
} from "./bundle";

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

const checkpoint = (seq: number, id: string, branch: string, manifest: string) =>
  ev(seq, {
    kind: "state.checkpoint.created",
    payload: {
      checkpoint_id: id,
      branch_id: branch,
      parent_id: null,
      after_call_id: null,
      manifest,
      files_changed: 1,
      bytes_added: 1,
      duration_ms: 1,
    },
  });

const events: Event[] = [
  ev(1, {
    kind: "meta.session.started",
    payload: {
      session_id: "01jbundle",
      agent_kind: "fakeagent",
      agent_version: null,
      command: ["agent"],
      workspace_root: "~/shop",
      mode: "cell",
      kernel_release: "6.17",
      capabilities: {},
      policy_set_hash: null,
    },
  }),
  checkpoint(2, "c1", "a", "b3:m1"),
  checkpoint(3, "c2", "a", "b3:m2"),
  ev(4, {
    kind: "state.checkpoint.forked",
    payload: { checkpoint_id: "c1", branch_id: "b", name: "try" },
  }),
];

const passed = (stage: string) => ({ stage, status: "passed", detail: null });

const good: BundleReport = {
  ok: true,
  checks: ["archive", "signature", "manifest", "files", "chain"].map(
    passed,
  ) as BundleReport["checks"],
  bundle: {
    session: { id: "01jbundle", name: "cart", agent: "fakeagent" },
    redaction: "strict",
    content: { included: true, chunks: 4, withheld_files: 0 },
    events: 4,
    head_hash: "b3:head",
    manifest_hash: "b3:manifest",
    signer_fingerprint: "0123456789abcdef",
  },
  error: null,
};

function view(report: BundleReport, calls: string[] = []): BundleView {
  return {
    report: () => JSON.stringify(report),
    events: () => JSON.stringify(events),
    diff: (a, b) => {
      calls.push(`diff ${a} ${b}`);
      return JSON.stringify({
        changes: [{ path: "src/cart.ts", kind: "modified", entry: "file" }],
      });
    },
    file: (a, b, path) => {
      calls.push(`file ${a} ${b} ${path}`);
      const side = (text: string) => ({
        source: "checkpoint",
        text,
        binary: false,
        truncated: false,
      });
      return JSON.stringify({ path, before: side("<b>0</b>"), after: side("1") });
    },
  };
}

describe("bundle mode", () => {
  it("recognises a bundle by its zstd magic", () => {
    expect(isBundle(new Uint8Array([0x28, 0xb5, 0x2f, 0xfd, 0]))).toBe(true);
    expect(isBundle(new TextEncoder().encode('{"seq":1}'))).toBe(false);
    expect(isBundle(new Uint8Array([0x28]))).toBe(false);
  });

  it("serves a verified bundle: session, verify report, diffs by checkpoint or branch", async () => {
    const calls: string[] = [];
    const { client, report } = createBundleClient(view(good, calls), "x.wyrd", null);
    expect(report.bundle.signer_fingerprint).toBe("0123456789abcdef");
    const sid = (await client.sessions()).sessions[0]?.id;
    expect(sid).toBe("01jbundle");
    const s = await client.session("01jbundle");
    expect(s.name).toBe("cart");
    expect(s.head_hash).toBe("b3:head");
    const v = await client.verify("01jbundle");
    expect(v).toMatchObject({ ok: true, sealed: true, signer_fingerprint: "0123456789abcdef" });
    expect((await client.diff("01jbundle", "c1", "c2")).changes).toHaveLength(1);
    // A branch resolves to its newest checkpoint, a fresh fork to its fork point.
    await client.diff("01jbundle", "b", "a");
    await client.diff("01jbundle", "c1", "head");
    const f = await client.diffFile("01jbundle", "c1", "c2", "src/cart.ts");
    expect(f.before.text).toBe("<b>0</b>");
    expect(calls).toEqual([
      "diff b3:m1 b3:m2",
      "diff b3:m1 b3:m2",
      "diff b3:m1 b3:m1",
      "file b3:m1 b3:m2 src/cart.ts",
    ]);
    await expect(client.diff("01jbundle", "nope", "c1")).rejects.toThrow(/no checkpoint or branch/);
    await expect(client.diff("other", "c1", "c2")).rejects.toThrow(/not in this file/);
    await expect(client.rewind("01jbundle", "c1", { relaunch: false })).rejects.toThrow(
      /read-only/,
    );
  });

  it("exposes nothing from a bundle that did not verify", () => {
    const bad: BundleReport = {
      ok: false,
      checks: [
        passed("archive"),
        passed("signature"),
        passed("manifest"),
        { stage: "files", status: "failed", detail: "event modified at seq 3" },
        { stage: "chain", status: "skipped", detail: null },
      ] as BundleReport["checks"],
      bundle: null,
      error: { stage: "files", seq: 3, message: "event modified at seq 3" },
    };
    let read = false;
    const v = { ...view(bad), events: () => ((read = true), "[]") };
    let err: unknown;
    try {
      createBundleClient(v, "x.wyrd", null);
    } catch (e) {
      err = e;
    }
    expect(err).toBeInstanceOf(BundleError);
    expect((err as BundleError).message).toMatch(/file hashes failed: event modified at seq 3/);
    expect((err as BundleError).report.checks[3]?.status).toBe("failed");
    expect(read).toBe(false);
  });

  it("opens bundles with the WebAssembly reader and exports without it", async () => {
    const bytes = new Uint8Array([0x28, 0xb5, 0x2f, 0xfd]);
    await expect(openSessionFile("x.wyrd", bytes, null)).rejects.toThrow(/WebAssembly/);
    const wasm = {
      correlate: () => "{}",
      BundleView: class {
        constructor(readonly bytes: Uint8Array) {}
        report = () => JSON.stringify(good);
        events = () => JSON.stringify(events);
        diff = () => "{}";
        file = () => "{}";
      },
    };
    expect((await openSessionFile("x.wyrd", bytes, wasm)).bundle?.session.id).toBe("01jbundle");
    const exported = new TextEncoder().encode(events.map((e) => JSON.stringify(e)).join("\n"));
    expect((await openSessionFile("x.jsonl", exported, null)).bundle).toBeNull();
  });
});
