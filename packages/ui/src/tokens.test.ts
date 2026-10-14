import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { contrast } from "./contrast";
import { meaning, solidColors, surfaces, text, translucent } from "./tokens";

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
});
