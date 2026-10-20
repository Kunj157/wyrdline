import type { Event } from "@wyrdline/schemas";

import { evidenceSeqs, groupByRule, projectDivergences } from "./divergences";

function ev(seq: number, body: Event["body"]): Event {
  return { seq, ts_mono_ns: seq, ts_wall_ms: seq, process: null, drops_since_last: 0, body };
}

const raised = (seq: number, id: string, rule: string): Event =>
  ev(seq, {
    kind: "verdict.divergence.raised",
    payload: {
      divergence_id: id,
      rule_id: rule,
      severity: "high",
      claim_id: rule === "D001" ? "cl-1" : null,
      evidence_seqs: [1],
      confidence_bp: 9000,
      summary: `${rule} fired`,
    },
  });

describe("divergences from the ledger", () => {
  it("folds triage updates and joins the claim, like the daemon", () => {
    const events: Event[] = [
      ev(1, {
        kind: "verdict.claim.extracted",
        payload: {
          claim_id: "cl-1",
          turn: 3,
          claim_type: "tests_pass",
          subject: "tests",
          quote: "All tests pass.",
          extractor: "rule",
        },
      }),
      raised(2, "dv-a", "D001"),
      raised(3, "dv-b", "D099"),
      ev(4, {
        kind: "verdict.divergence.updated",
        payload: { divergence_id: "dv-a", status: "acknowledged", note: "first" },
      }),
      raised(5, "dv-a", "D001"),
      ev(6, {
        kind: "verdict.divergence.updated",
        payload: { divergence_id: "dv-a", status: "false_positive", note: "second" },
      }),
      ev(7, {
        kind: "verdict.divergence.updated",
        payload: { divergence_id: "dv-unknown", status: "acknowledged", note: null },
      }),
    ];
    const out = projectDivergences(events);
    expect(out.map((d) => [d.seq, d.divergence_id, d.rule_name, d.status, d.note])).toEqual([
      [2, "dv-a", "claim-without-evidence", "false_positive", "second"],
      [3, "dv-b", "unknown", "open", null],
    ]);
    expect(out[0]?.claim?.quote).toBe("All tests pass.");
    expect(out[1]?.claim).toBeNull();
  });

  it("cites evidence of every divergence not marked a false positive", () => {
    const views = projectDivergences([
      raised(2, "dv-a", "D001"),
      raised(3, "dv-b", "D002"),
      ev(4, {
        kind: "verdict.divergence.updated",
        payload: { divergence_id: "dv-b", status: "false_positive", note: null },
      }),
    ]).map((d, i) => ({ ...d, evidence_seqs: [10 + i, 20] }));
    expect([...evidenceSeqs(views)].sort()).toEqual([10, 20]);
  });

  it("groups across sessions by rule, most severe first", () => {
    const [a, b] = projectDivergences([raised(2, "dv-a", "D002"), raised(3, "dv-b", "D001")]);
    if (!a || !b) throw new Error("projection");
    const groups = groupByRule([
      { sid: "s1", divergence: { ...a, severity: "low" } },
      { sid: "s2", divergence: b },
      { sid: "s2", divergence: { ...a, divergence_id: "dv-c", severity: "critical" } },
    ]);
    expect(groups.map((g) => [g.rule_id, g.rule_name])).toEqual([
      ["D001", "claim-without-evidence"],
      ["D002", "out-of-scope-write"],
    ]);
    expect(groups[1]?.entries.map((e) => [e.sid, e.divergence.severity])).toEqual([
      ["s2", "critical"],
      ["s1", "low"],
    ]);
  });
});
