/**
 * Columnar river data for the GPU and canvas renderers: typed arrays, no
 * per-event objects on the drawing path (plan §16.4).
 */
import type { Meaning } from "@wyrdline/ui";

import type { RiverLayout } from "../lib/model";

export const LANES = ["intent", "spine", "effect"] as const;
export const MEANINGS: Meaning[] = [
  "intent",
  "effect",
  "divergence",
  "denied",
  "irreversible",
  "ok",
  "unknown",
];

export interface Columns {
  count: number;
  /** World x of each knot. */
  x: Float32Array;
  /** Lane index into LANES. */
  lane: Uint8Array;
  /** Meaning index into MEANINGS. */
  meaning: Uint8Array;
  seq: Uint32Array;
  /** Thread endpoints as knot indices. */
  threadFrom: Uint32Array;
  threadTo: Uint32Array;
  threadAmbiguous: Uint8Array;
  /** Index of the call each knot belongs to (threads light up per call); -1 none. */
  callIndex: Int32Array;
  width: number;
}

export function toColumns(layout: RiverLayout): Columns {
  const n = layout.knots.length;
  const x = new Float32Array(n);
  const lane = new Uint8Array(n);
  const meaning = new Uint8Array(n);
  const seq = new Uint32Array(n);
  const callIndex = new Int32Array(n).fill(-1);
  const index = new Map<number, number>();
  const calls = new Map<string, number>();
  layout.knots.forEach((k, i) => {
    x[i] = k.x;
    lane[i] = LANES.indexOf(k.lane);
    meaning[i] = Math.max(0, MEANINGS.indexOf(k.meaning));
    seq[i] = k.seq;
    index.set(k.seq, i);
    if (k.callId) {
      if (!calls.has(k.callId)) calls.set(k.callId, calls.size);
      callIndex[i] = calls.get(k.callId) ?? -1;
    }
  });
  const threads = layout.threads.filter((t) => index.has(t.from) && index.has(t.to));
  const threadFrom = new Uint32Array(threads.length);
  const threadTo = new Uint32Array(threads.length);
  const threadAmbiguous = new Uint8Array(threads.length);
  threads.forEach((t, i) => {
    threadFrom[i] = index.get(t.from) ?? 0;
    threadTo[i] = index.get(t.to) ?? 0;
    threadAmbiguous[i] = t.ambiguous ? 1 : 0;
  });
  return {
    count: n,
    x,
    lane,
    meaning,
    seq,
    threadFrom,
    threadTo,
    threadAmbiguous,
    callIndex,
    width: layout.width,
  };
}

/** World → screen: screenX = (worldX - offset) * scale. */
export interface Camera {
  offset: number;
  scale: number;
}

export function clampCamera(cam: Camera, worldWidth: number, viewWidth: number): Camera {
  const scale = Math.min(8, Math.max(0.002, cam.scale));
  const maxOffset = Math.max(0, worldWidth - viewWidth / scale);
  return { scale, offset: Math.min(maxOffset, Math.max(0, cam.offset)) };
}

/** Zoom around a screen x, keeping the world point under it fixed. */
export function zoomAt(cam: Camera, factor: number, screenX: number): Camera {
  const world = cam.offset + screenX / cam.scale;
  const scale = cam.scale * factor;
  return { scale, offset: world - screenX / scale };
}

/** Indices of knots whose x falls in the visible world range (x is sorted). */
export function visibleRange(
  c: Columns,
  cam: Camera,
  viewWidth: number,
  margin = 16,
): [number, number] {
  const lo = cam.offset - margin / cam.scale;
  const hi = cam.offset + (viewWidth + margin) / cam.scale;
  return [lowerBound(c.x, lo), upperBound(c.x, hi)];
}

function upperBound(a: Float32Array, v: number): number {
  let lo = 0;
  let hi = a.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if ((a[mid] ?? 0) <= v) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function lowerBound(a: Float32Array, v: number): number {
  let lo = 0;
  let hi = a.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if ((a[mid] ?? 0) < v) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** Level of detail: past this many visible knots in one lane, draw buckets. */
export const LOD_THRESHOLD = 2000;

export interface Bucket {
  lane: number;
  /** Screen x of the bucket's left edge. */
  sx: number;
  count: number;
  /** Most severe meaning in the bucket (index into MEANINGS). */
  meaning: number;
}

const SEVERITY = [0, 1, 6, 5, 4, 3, 2]; // intent<effect<unknown<ok<irreversible<denied<divergence

/** Aggregate knots of lane `lane` in [from, to) into `px`-wide screen buckets. */
export function aggregate(
  c: Columns,
  cam: Camera,
  from: number,
  to: number,
  lane: number,
  px = 3,
): Bucket[] {
  const out = new Map<number, Bucket>();
  for (let i = from; i < to; i++) {
    if (c.lane[i] !== lane) continue;
    const sx = Math.floor((((c.x[i] ?? 0) - cam.offset) * cam.scale) / px) * px;
    const m = c.meaning[i] ?? 0;
    const b = out.get(sx);
    if (!b) out.set(sx, { lane, sx, count: 1, meaning: m });
    else {
      b.count += 1;
      if ((SEVERITY[m] ?? 0) > (SEVERITY[b.meaning] ?? 0)) b.meaning = m;
    }
  }
  return [...out.values()];
}

/** Knot nearest to a screen point within `radius` px, or -1. */
export function hitTest(
  c: Columns,
  cam: Camera,
  sx: number,
  sy: number,
  laneY: (lane: number) => number,
  radius = 9,
): number {
  const [from, to] = visibleRange(c, cam, 0, radius);
  const centre = lowerBound(c.x, cam.offset + sx / cam.scale);
  let best = -1;
  let bestD = radius * radius;
  for (let i = Math.max(from, centre - 64); i < Math.min(c.count, Math.max(to, centre + 64)); i++) {
    const dx = ((c.x[i] ?? 0) - cam.offset) * cam.scale - sx;
    const dy = laneY(c.lane[i] ?? 0) - sy;
    const d = dx * dx + dy * dy;
    if (d <= bestD) {
      bestD = d;
      best = i;
    }
  }
  return best;
}
