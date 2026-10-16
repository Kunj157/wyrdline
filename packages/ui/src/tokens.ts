/**
 * "Verdigris Daylight" design tokens — the single source of truth for
 * colour, radius, elevation, and motion, shared by Studio and the landing
 * page. Bright, never dark and never plain white: warm cream surfaces, deep
 * ink-violet text, saturated meaning colours, and vivid decorative hues.
 * `theme.css` mirrors these values as CSS variables (a test keeps the two
 * in sync) and the canvas/WebGPU renderers import them directly.
 * Components never use raw hex values.
 */

export const surfaces = {
  background: "#fbf1e1",
  "surface-1": "#fffaf1",
  "surface-2": "#f7e9d3",
  "surface-3": "#f1e1c8",
} as const;

export const text = {
  "text-primary": "#1c1830",
  "text-secondary": "#3f3756",
  "text-muted": "#585070",
} as const;

/**
 * Colours that carry meaning, at text strength (≥ 4.5:1 on every surface).
 * Meaning is never colour alone: always paired with an icon and a label.
 */
export const meaning = {
  accent: "#006c62",
  "accent-hover": "#00574f",
  effect: "#75532a",
  "verdict-divergence": "#a8380b",
  "verdict-denied": "#b3202a",
  "verdict-irreversible": "#7d5300",
  "verdict-ok": "#166c37",
  "verdict-unknown": "#585070",
} as const;

/**
 * Decorative hues for fills, gradients, glows, and 3D materials. Never body
 * text and never the only carrier of meaning, so exempt from the text
 * contrast check.
 */
export const vivid = {
  "vivid-verdigris": "#12c9a8",
  "vivid-bone": "#f3d9a4",
  "vivid-vermilion": "#ff5f2e",
  "vivid-ochre": "#ffb21f",
  "vivid-red": "#ff3b4f",
  "vivid-green": "#2fd36f",
  "vivid-violet": "#7b5cff",
  "vivid-sky": "#2fa8ff",
} as const;

export const translucent = {
  "surface-glass": "rgba(255, 250, 241, 0.72)",
  border: "rgba(28, 24, 48, 0.1)",
  "border-strong": "rgba(28, 24, 48, 0.18)",
  "accent-subtle": "rgba(18, 201, 168, 0.16)",
} as const;

export const onAccent = "#ffffff";

export const radius = { sm: "6px", card: "12px", lg: "18px" } as const;

export const shadows = {
  "shadow-1": "0 1px 2px rgba(28, 24, 48, 0.1)",
  "shadow-2": "0 6px 20px rgba(123, 92, 255, 0.1), 0 1px 2px rgba(28, 24, 48, 0.12)",
} as const;

export const motion = {
  "duration-fast": "120ms",
  "duration-base": "200ms",
  "ease-out": "cubic-bezier(0.16, 1, 0.3, 1)",
} as const;

export type Hex = `#${string}`;

/** Solid colour tokens by CSS variable name (without `--color-`). */
export const solidColors: Record<string, string> = {
  ...surfaces,
  ...text,
  ...meaning,
  ...vivid,
  "on-accent": onAccent,
};
