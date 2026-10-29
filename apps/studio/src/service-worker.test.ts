import { describe, expect, it } from "vitest";

import { serviceWorkerSource } from "../service-worker";

describe("service worker", () => {
  const assets = ["assets/index-abc.js", "assets/wyrd_wasm_bg-def.wasm", "wyrdline.svg"];

  it("precaches the app shell and every built file", () => {
    const src = serviceWorkerSource(assets);
    expect(src).toContain('cache.addAll(["./", ...ASSETS])');
    for (const a of assets) expect(src).toContain(JSON.stringify(a));
    // It parses as a script.
    expect(() => new Function(src)).not.toThrow();
  });

  it("never caches or answers the daemon API, the Hub API or served bundles", () => {
    const src = serviceWorkerSource(assets);
    expect(src).toMatch(
      /path\.startsWith\("v1\/"\) \|\| path\.startsWith\("api\/"\) \|\| path\.startsWith\("bundle\/"\)\) return;/,
    );
  });

  it("gets a new cache name when the build changes, and only then", () => {
    const version = (s: string) => /wyrd-studio-([0-9a-f]+)/.exec(s)?.[1];
    const a = version(serviceWorkerSource(assets));
    expect(version(serviceWorkerSource([...assets].reverse()))).toBe(a);
    expect(version(serviceWorkerSource([...assets, "assets/new-123.js"]))).not.toBe(a);
  });
});
