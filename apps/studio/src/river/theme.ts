/** Renderer colours, read from the shared tokens (no raw hex in components). */
import { meaning, text, vivid } from "@wyrdline/ui";

import { MEANINGS } from "./data";

export type Rgba = [number, number, number, number];

export function rgba(hex: string, alpha = 1): Rgba {
  const n = Number.parseInt(hex.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, alpha];
}

const byMeaning: Record<(typeof MEANINGS)[number], string> = {
  intent: meaning.accent,
  effect: meaning.effect,
  divergence: meaning["verdict-divergence"],
  denied: meaning["verdict-denied"],
  irreversible: meaning["verdict-irreversible"],
  ok: meaning["verdict-ok"],
  unknown: meaning["verdict-unknown"],
};

export const palette = {
  knots: MEANINGS.map((m) => rgba(byMeaning[m])),
  spine: rgba(vivid["vivid-violet"]),
  thread: rgba(text["text-muted"], 0.35),
  threadLit: rgba(meaning.accent, 0.95),
  ring: rgba(text["text-primary"]),
};

export const css = (c: Rgba) =>
  `rgba(${Math.round(c[0] * 255)}, ${Math.round(c[1] * 255)}, ${Math.round(c[2] * 255)}, ${c[3]})`;

/** CSS-pixel y of each lane (index into LANES) in a river of height 196. */
export const LANE_Y = [36, 96, 156] as const;
export const RIVER_HEIGHT = 196;
