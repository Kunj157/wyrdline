/**
 * "Ink & Verdigris" design tokens — the single source of truth for colour,
 * radius, and motion. `theme.css` mirrors these values as CSS variables
 * (a test keeps the two in sync) and the WebGPU renderers import them
 * directly. Components never use raw hex values.
 */

export const surfaces = {
  background: "#0e0d0c",
  "surface-1": "#151413",
  "surface-2": "#1c1a18",
  "surface-3": "#252220",
} as const;

export const text = {
  "text-primary": "#efeae3",
  "text-secondary": "#b3aca2",
  "text-muted": "#8f897f",
} as const;

/** Colours that carry meaning; always paired with an icon and a label. */
export const meaning = {
  accent: "#4fd1b5",
  "accent-hover": "#6fe0c8",
  effect: "#d9cfbf",
  "verdict-divergence": "#ff7a4d",
  "verdict-denied": "#f05a5a",
  "verdict-irreversible": "#e3a83b",
  "verdict-ok": "#63c78c",
  "verdict-unknown": "#8f897f",
} as const;

export const translucent = {
  "surface-glass": "rgba(28, 26, 24, 0.74)",
  border: "rgba(255, 245, 230, 0.08)",
  "border-strong": "rgba(255, 245, 230, 0.15)",
  "accent-subtle": "rgba(79, 209, 181, 0.12)",
} as const;

export const onAccent = "#0e0d0c";

export const radius = { sm: "6px", card: "12px", lg: "18px" } as const;

export const motion = {
  "duration-fast": "120ms",
  "duration-base": "200ms",
  "ease-out": "cubic-bezier(0.16, 1, 0.3, 1)",
} as const;

export type Hex = `#${string}`;

/** All solid colour tokens by CSS variable name (without `--`). */
export const solidColors: Record<string, string> = {
  ...surfaces,
  ...text,
  ...meaning,
  "on-accent": onAccent,
};
