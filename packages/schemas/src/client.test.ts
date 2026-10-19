import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { ApiError, createClient } from "./client";

interface Call {
  url: string;
  init: RequestInit;
}

function fake(status: number, body: unknown) {
  const calls: Call[] = [];
  const fetch = (async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return new Response(body === undefined ? "" : JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof globalThis.fetch;
  return { calls, fetch };
}

describe("daemon client", () => {
  it("builds urls, query strings, and auth headers", async () => {
    const f = fake(200, { events: [], next_after_seq: null });
    const c = createClient({ baseUrl: "http://127.0.0.1:7717", token: "t0k", fetch: f.fetch });
    await c.events("01abc", { plane: "effect", after_seq: 10, limit: undefined });
    const call = f.calls[0];
    expect(call?.url).toBe(
      "http://127.0.0.1:7717/v1/sessions/01abc/events?plane=effect&after_seq=10",
    );
    expect((call?.init.headers as Record<string, string>).authorization).toBe("Bearer t0k");
  });

  it("sends bodies and idempotency keys on mutations", async () => {
    const f = fake(201, { branch: { id: "b", name: "x", forked_from: "c1", created_ms: 1 } });
    const c = createClient({ fetch: f.fetch });
    await c.fork("01abc", "c1", { name: "x", relaunch: false }, "key-1");
    const call = f.calls[0];
    expect(call?.url).toBe("/v1/sessions/01abc/checkpoints/c1/fork");
    expect(call?.init.method).toBe("POST");
    expect(JSON.parse(String(call?.init.body))).toEqual({ name: "x", relaunch: false });
    const headers = call?.init.headers as Record<string, string>;
    expect(headers["idempotency-key"]).toBe("key-1");
    expect(headers.authorization).toBeUndefined();
  });

  it("turns problems into ApiError", async () => {
    const problem = { type: "not-found", title: "Not found", status: 404, detail: "session zz" };
    const c = createClient({ fetch: fake(404, problem).fetch });
    const err = await c.session("zz").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).problem.type).toBe("not-found");
    expect((err as ApiError).status).toBe(404);
  });

  it("escapes path segments", () => {
    const c = createClient({});
    expect(c.streamUrl("01abc", 5)).toBe("/v1/sessions/01abc/stream?after_seq=5");
    expect(c.streamUrl("../x")).toBe("/v1/sessions/..%2Fx/stream");
  });

  it("only calls routes the OpenAPI document defines", async () => {
    const doc = JSON.parse(
      readFileSync(new URL("../../../docs/api/openapi.json", import.meta.url), "utf8"),
    ) as { paths: Record<string, Record<string, unknown>> };
    const f = fake(200, {});
    const c = createClient({ fetch: f.fetch });
    await Promise.all([
      c.health(),
      c.ready(),
      c.capabilities(),
      c.sessions(),
      c.session("s"),
      c.events("s"),
      c.graph("s"),
      c.toolCall("s", "t"),
      c.verify("s"),
      c.checkpoints("s"),
      c.checkpoint("s"),
      c.stop("s"),
      c.asks("s"),
      c.answerAsk("s", 1, true),
      c.rewind("s", "c1", {}),
      c.fork("s", "c1", { name: "n" }),
      c.apply("s", "c1", {}),
      c.diff("s", "a", "b"),
      c.diffFile("s", "a", "b", "src/x.ts"),
    ]);
    // Segment-wise match: `{param}` matches any one segment.
    const matches = (template: string, path: string) => {
      const t = template.split("/");
      const p = path.split("/");
      return t.length === p.length && t.every((seg, i) => seg.startsWith("{") || seg === p[i]);
    };
    const templates = Object.keys(doc.paths);
    for (const { url, init } of f.calls) {
      const path = url.split("?")[0] ?? "";
      const match = templates.find((t) => matches(t, path));
      expect(match, path).toBeDefined();
      const method = (init.method ?? "GET").toLowerCase();
      expect(doc.paths[match ?? ""]?.[method], `${method} ${path}`).toBeDefined();
    }
  });
});
