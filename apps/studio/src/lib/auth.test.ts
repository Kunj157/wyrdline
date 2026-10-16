import { describe, expect, it } from "vitest";

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
