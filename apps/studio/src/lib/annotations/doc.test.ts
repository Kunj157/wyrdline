import * as A from "@automerge/automerge/slim";
import { beforeAll, describe, expect, it } from "vitest";

import {
  addComment,
  createAnnotations,
  deleteComment,
  editComment,
  LIMITS,
  loadAnnotations,
  pinCheckpoint,
  resolveComment,
  setTriage,
  unpinCheckpoint,
  viewAnnotations,
} from "./doc";
import { loadAutomergeForTests } from "./test-wasm";

beforeAll(loadAutomergeForTests);

describe("annotation documents", () => {
  it("starts every replica from the same root change", () => {
    const a = createAnnotations("s1");
    const b = createAnnotations("s1");
    expect(A.getHeads(a)).toEqual(A.getHeads(b));
    expect(A.getActorId(a)).not.toEqual(A.getActorId(b));
    expect(A.getHeads(createAnnotations("s2"))).not.toEqual(A.getHeads(a));
  });

  it("anchors comments to seq ranges and edits them", () => {
    let d = createAnnotations("s1");
    d = addComment(d, { from: 9, to: 12, text: " curl after .env ", author: "ana" }, "c1");
    d = addComment(d, { from: 3, to: 3, text: "first read", author: "bo" }, "c2");
    d = editComment(d, "c1", "curl right after reading .env");
    d = resolveComment(d, "c2", true);
    const v = viewAnnotations(d);
    expect(v.comments.map((c) => [c.id, c.from, c.to, c.text, c.resolved])).toEqual([
      ["c2", 3, 3, "first read", true],
      ["c1", 9, 12, "curl right after reading .env", false],
    ]);
    d = deleteComment(d, "c2");
    expect(viewAnnotations(d).comments.map((c) => c.id)).toEqual(["c1"]);
  });

  it("rejects malformed comments before writing them", () => {
    const d = createAnnotations("s1");
    expect(() => addComment(d, { from: 5, to: 4, text: "x", author: "a" })).toThrow(/seq range/);
    expect(() => addComment(d, { from: -1, to: 4, text: "x", author: "a" })).toThrow(/seq range/);
    expect(() => addComment(d, { from: 1, to: 1, text: "  ", author: "a" })).toThrow(/empty/);
    expect(() =>
      addComment(d, { from: 1, to: 1, text: "x".repeat(LIMITS.text + 1), author: "a" }),
    ).toThrow(/longer/);
  });

  it("keeps triage state and pinned checkpoints", () => {
    let d = createAnnotations("s1");
    d = setTriage(d, "dv1", "false_positive", "ana", "fixture, not a secret");
    d = pinCheckpoint(d, "cp9", "before the injection", "bo");
    d = pinCheckpoint(d, "cp3", "tests green", "bo");
    d = unpinCheckpoint(d, "cp3");
    const v = viewAnnotations(d);
    expect(v.triage.get("dv1")).toMatchObject({ state: "false_positive", by: "ana" });
    expect([...v.pins.keys()]).toEqual(["cp9"]);
  });

  it("round-trips through save and load", () => {
    let d = createAnnotations("s1");
    d = addComment(d, { from: 1, to: 2, text: "hi", author: "a" }, "c1");
    const back = loadAnnotations(A.save(d));
    expect(viewAnnotations(back)).toEqual(viewAnnotations(d));
  });

  it("leaves out entries a hostile replica wrote in the wrong shape", () => {
    let d = createAnnotations("s1");
    d = A.change(d, (x) => {
      const raw = x as unknown as Record<"comments" | "triage" | "pins", Record<string, unknown>>;
      raw.comments.bad = { from: "9", to: 1, text: "x", author: "a", at: 1 };
      raw.comments.inverted = { from: 9, to: 1, text: "x", author: "a", at: 1 };
      raw.triage.weird = { state: "deleted", by: "a", note: "", at: 1 };
      raw.pins.huge = { label: "x".repeat(LIMITS.label + 1), by: "a", at: 1 };
    });
    const v = viewAnnotations(d);
    expect(v.comments).toEqual([]);
    expect(v.triage.size).toBe(0);
    expect(v.pins.size).toBe(0);
  });
});
