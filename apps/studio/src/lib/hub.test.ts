import { afterEach, describe, expect, it, vi } from "vitest";

import { HubError, hubGet, loginUrl, percent, share } from "./hub";

afterEach(() => vi.unstubAllGlobals());

describe("hub client", () => {
  it("formats rates and bar shares", () => {
    expect(percent(2 / 3)).toBe("67 %");
    expect(share(0, 10)).toBe(0);
    expect(share(1, 1000)).toBe(2);
    expect(share(5, 10)).toBe(50);
    expect(share(3, 0)).toBe(0);
  });

  it("builds the login URL with a local return path", () => {
    expect(loginUrl("/hub?org=a b")).toBe("/api/v1/auth/oidc/login?return_to=%2Fhub%3Forg%3Da%20b");
  });

  it("turns problem details into errors with the position", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(
          new Response(JSON.stringify({ title: "Bad request", detail: "x", position: 7 }), {
            status: 400,
          }),
        ),
      ),
    );
    const e = await hubGet("/orgs/a/search?q=x").catch((err: unknown) => err);
    expect(e).toBeInstanceOf(HubError);
    expect((e as HubError).position).toBe(7);
    expect((e as HubError).status).toBe(400);
  });
});
