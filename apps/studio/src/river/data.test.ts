import { describe, expect, it } from "vitest";

import type { RiverLayout } from "../lib/model";
import { aggregate, clampCamera, hitTest, MEANINGS, toColumns, visibleRange, zoomAt } from "./data";

function layout(n: number): RiverLayout {
  const knots = Array.from({ length: n }, (_, i) => ({
    seq: i + 1,
    lane: (i % 3 === 0 ? "intent" : "effect") as "intent" | "effect",
    x: (i + 1) * 10,
    meaning: (i === 7 ? "denied" : "effect") as "denied" | "effect",
    label: `k${i}`,
    callId: i % 3 === 0 ? `c${i}` : `c${i - (i % 3)}`,
  }));
  return {
    knots,
    threads: [{ from: 1, to: 2, callId: "c0", ambiguous: false }],
    width: (n + 1) * 10,
  };
}

describe("river columns", () => {
  it("packs knots and threads into typed arrays", () => {
    const c = toColumns(layout(10));
    expect(c.count).toBe(10);
    expect(Array.from(c.x.slice(0, 3))).toEqual([10, 20, 30]);
    expect(c.lane[0]).toBe(0);
    expect(c.lane[1]).toBe(2);
    expect(MEANINGS[c.meaning[7] ?? 0]).toBe("denied");
    expect([c.threadFrom[0], c.threadTo[0]]).toEqual([0, 1]);
    expect(c.callIndex[1]).toBe(c.callIndex[0]);
  });

  it("zooms around the cursor and clamps the camera", () => {
    const z = zoomAt({ offset: 0, scale: 1 }, 2, 100);
    expect(z.offset + 100 / z.scale).toBeCloseTo(100);
    expect(clampCamera({ offset: -50, scale: 100 }, 1000, 500)).toEqual({ offset: 0, scale: 8 });
  });

  it("finds the visible range by binary search", () => {
    const c = toColumns(layout(1000));
    const [from, to] = visibleRange(c, { offset: 500, scale: 1 }, 100, 0);
    expect(c.x[from]).toBe(500);
    expect(c.x[to - 1]).toBe(600);
  });

  it("aggregates dense lanes into buckets keeping the most severe meaning", () => {
    const c = toColumns(layout(30));
    const buckets = aggregate(c, { offset: 0, scale: 0.1 }, 0, c.count, 2, 3);
    expect(buckets.reduce((n, b) => n + b.count, 0)).toBe(20);
    expect(buckets.some((b) => MEANINGS[b.meaning] === "denied")).toBe(true);
  });

  it("hit-tests the nearest knot", () => {
    const c = toColumns(layout(10));
    const laneY = (l: number) => [30, 90, 150][l] ?? 0;
    expect(hitTest(c, { offset: 0, scale: 1 }, 21, 151, laneY)).toBe(1);
    expect(hitTest(c, { offset: 0, scale: 1 }, 21, 30, laneY)).toBe(-1);
  });
});

describe("thread culling", () => {
  it("finds the threads ending in view and a call's threads", async () => {
    const { toColumns: cols, visibleThreads, threadsOfCall } = await import("./data");
    const layout = {
      knots: [0, 1, 2, 3, 4].map((i) => ({
        seq: i + 1,
        lane: (i === 0 || i === 3 ? "intent" : "effect") as "intent" | "effect",
        x: (i + 1) * 10,
        meaning: "effect" as const,
        label: "",
        callId: i < 3 ? "a" : "b",
      })),
      threads: [
        { from: 4, to: 5, callId: "b", ambiguous: false },
        { from: 1, to: 2, callId: "a", ambiguous: false },
        { from: 1, to: 3, callId: "a", ambiguous: false },
      ],
      width: 60,
    };
    const c = cols(layout);
    expect(Array.from(c.threadTo)).toEqual([1, 2, 4]);
    expect(visibleThreads(c, 2, 5)).toEqual([1, 3]);
    expect(threadsOfCall(c, c.callIndex[0] ?? -1)).toEqual([0, 1]);
  });
});
