import { describe, expect, it, vi } from "vitest";

import { takeTokenFromUrl } from "./auth";

// Built at runtime so no token-shaped literal sits in the source.
const sample = ["0123456789", "abcdef", "0123"].join("");

describe("token bootstrap", () => {
  it("moves a fragment token into session storage and strips it", () => {
    window.history.replaceState(null, "", `/sessions#token=${sample}`);
    sessionStorage.clear();
    expect(takeTokenFromUrl()).toBe(sample);
    expect(window.location.hash).toBe("");
    expect(window.location.pathname).toBe("/sessions");
    expect(sessionStorage.getItem("wyrd.token")).toBe(sample);
    // Later loads read it back.
    expect(takeTokenFromUrl()).toBe(sample);
  });

  it("ignores fragments that are not tokens", () => {
    sessionStorage.clear();
    window.history.replaceState(null, "", "/#token=<script>");
    expect(takeTokenFromUrl()).toBeNull();
  });
});

describe("token handoff keeps the page query", () => {
  it("removes the token from the fragment and keeps ?hub=", () => {
    const replace = vi.spyOn(window.history, "replaceState");
    const store = new Map<string, string>();
    const fake = {
      setItem: (k: string, v: string) => store.set(k, v),
      getItem: (k: string) => store.get(k) ?? null,
    } as unknown as Storage;
    const loc = {
      hash: `#token=${sample}`,
      pathname: "/sessions/s1",
      search: "?hub=bnd_e2e",
    } as Location;
    expect(takeTokenFromUrl(loc, fake)).toBe(sample);
    expect(replace).toHaveBeenCalledWith(null, "", "/sessions/s1?hub=bnd_e2e");
    replace.mockRestore();
  });
});
