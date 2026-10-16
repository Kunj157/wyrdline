// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { contrast } from "./contrast";
import { meaning, shadows, solidColors, surfaces, text, translucent, vivid } from "./tokens";

describe("token contrast (WCAG AA)", () => {
  for (const [surfaceName, surface] of Object.entries(surfaces)) {
    for (const [name, value] of Object.entries(text)) {
      it(`${name} on ${surfaceName} ≥ 4.5`, () => {
        expect(contrast(value, surface)).toBeGreaterThanOrEqual(4.5);
      });
    }
    for (const [name, value] of Object.entries(meaning)) {
      it(`${name} on ${surfaceName} ≥ 4.5 (used as label text)`, () => {
        expect(contrast(value, surface)).toBeGreaterThanOrEqual(4.5);
      });
    }
  }

  it("the palette is bright: every surface is light", () => {
    for (const value of Object.values(surfaces)) {
      expect(contrast(value, "#000000")).toBeGreaterThan(15);
    }
  });

  it("vivid hues are decorative: none is also a text or meaning token", () => {
    const strong = new Set<string>([...Object.values(text), ...Object.values(meaning)]);
    for (const value of Object.values(vivid)) expect(strong.has(value)).toBe(false);
  });

  it("on-accent text on accent ≥ 4.5", () => {
    expect(contrast(solidColors["on-accent"] ?? "", meaning.accent)).toBeGreaterThanOrEqual(4.5);
  });
});

describe("theme.css mirrors tokens.ts", () => {
  const css = readFileSync(new URL("./theme.css", import.meta.url), "utf8");
  const all = { ...solidColors, ...translucent };
  for (const [name, value] of Object.entries(all)) {
    it(`--color-${name}`, () => {
      expect(css).toContain(`--color-${name}: ${value};`);
    });
  }
  for (const [name, value] of Object.entries(shadows)) {
    it(`--${name}`, () => {
      expect(css).toContain(`--${name}: ${value};`);
    });
  }
});
