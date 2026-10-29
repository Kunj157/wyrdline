import { describe, expect, it } from "vitest";

import { parseBacktestReport } from "./backtest";

const session = { id: "01jsess", events: 12 };
const report = {
  policy_id: "strict",
  policy_version: "b3:0123",
  session_id: "01jsess",
  events: 12,
  findings: [{ seq: 7, stage: "kernel", decision: "deny", reason: "connect 203.0.113.9:443" }],
  notes: ["n"],
};

describe("parseBacktestReport", () => {
  it("accepts a report of this session", () => {
    expect(parseBacktestReport(JSON.stringify(report), session)).toEqual(report);
  });

  it("keeps only known fields", () => {
    const extra = { ...report, findings: [{ ...report.findings[0], html: "<b>x</b>" }], x: 1 };
    const r = parseBacktestReport(JSON.stringify(extra), session);
    expect(r.findings[0]).not.toHaveProperty("html");
    expect(r).not.toHaveProperty("x");
  });

  it("refuses other sessions, bad seqs, and non-reports", () => {
    expect(() => parseBacktestReport("{", session)).toThrow(/not JSON/);
    expect(() => parseBacktestReport("[]", session)).toThrow(/not a backtest report/);
    expect(() =>
      parseBacktestReport(JSON.stringify({ ...report, session_id: "other" }), session),
    ).toThrow(/not this one/);
    expect(() => parseBacktestReport(JSON.stringify({ ...report, events: 13 }), session)).toThrow(
      /not this one/,
    );
    for (const seq of [0, 13, 1.5, "7"]) {
      const bad = { ...report, findings: [{ ...report.findings[0], seq }] };
      expect(() => parseBacktestReport(JSON.stringify(bad), session)).toThrow(/finding 1/);
    }
  });
});
