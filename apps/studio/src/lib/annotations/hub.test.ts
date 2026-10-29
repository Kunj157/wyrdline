import { describe, expect, it } from "vitest";

import { syncPath, wsUrl } from "./client";
import { bundleFromLocation } from "./use-annotations";

describe("Hub sync addressing", () => {
  it("builds the bundle's sync path and its ws(s) form on the page origin", () => {
    expect(syncPath("01JBUNDLE")).toBe("/api/v1/sync?bundle=01JBUNDLE");
    expect(wsUrl(syncPath("01JBUNDLE"), "https://hub.example.internal/sessions")).toBe(
      "wss://hub.example.internal/api/v1/sync?bundle=01JBUNDLE",
    );
    expect(wsUrl(syncPath("a b"), "https://hub.example.internal/")).toBe(
      "wss://hub.example.internal/api/v1/sync?bundle=a%20b",
    );
  });

  it("reads the bundle id from the page query, and refuses anything odd", () => {
    expect(bundleFromLocation("?hub=01JBUNDLE")).toBe("01JBUNDLE");
    expect(bundleFromLocation("")).toBeNull();
    expect(bundleFromLocation("?hub=")).toBeNull();
    expect(bundleFromLocation("?hub=../../admin")).toBeNull();
    expect(bundleFromLocation(`?hub=${"x".repeat(65)}`)).toBeNull();
  });
});
