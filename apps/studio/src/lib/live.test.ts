import { describe, expect, it } from "vitest";

import { parseSse } from "./live";

describe("SSE parsing", () => {
  it("splits complete messages, keeps the rest, skips comments", () => {
    const { messages, rest } = parseSse(
      ': keep-alive\n\nid: 7\nevent: ledger\ndata: {"seq":7}\n\nevent: end\ndata: 9\n\nid: 10\nevent: led',
    );
    expect(messages).toEqual([
      { event: "ledger", id: "7", data: '{"seq":7}' },
      { event: "end", id: null, data: "9" },
    ]);
    expect(rest).toBe("id: 10\nevent: led");
  });

  it("joins multi-line data and keeps colons in values", () => {
    const { messages } = parseSse("data: a:b\ndata: c\n\n");
    expect(messages[0]?.data).toBe("a:b\nc");
  });
});
