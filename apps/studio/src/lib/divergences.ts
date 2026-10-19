/**
 * Divergences as Studio shows them. The daemon projects them from the
 * ledger (`GET /v1/sessions/{sid}/divergences`); offline replay folds the
 * exported events here the same way (`wyrd_divergence::view::project`).
 */
import type { Event } from "@wyrdline/schemas";
import type * as api from "@wyrdline/schemas/generated-api";

/** Rule ids and names (plan §17.4, `wyrd_divergence::RULES`). */
export const RULE_NAMES: Record<string, string> = {
  D001: "claim-without-evidence",
  D002: "out-of-scope-write",
  D003: "secret-read-then-egress",
  D004: "unattributed-effect",
  D005: "silent-failure",
  D006: "test-tampering",
  D007: "hidden-network",
  D008: "mass-destruction",
  D009: "tool-rug-pull",
  D010: "claim-scope-mismatch",
};

/**
 * Every divergence raised in `events`, in ledger order, with its latest
 * triage (later updates win; a repeated raise is ignored).
 */
export function projectDivergences(events: Event[]): api.DivergenceView[] {
  const claims = new Map<string, api.ClaimExtracted>();
  const out: api.DivergenceView[] = [];
  for (const e of events) {
    const b = e.body;
    if (b.kind === "verdict.claim.extracted") {
      claims.set(b.payload.claim_id, b.payload);
    } else if (b.kind === "verdict.divergence.raised") {
      const d = b.payload;
      if (out.some((v) => v.divergence_id === d.divergence_id)) continue;
      out.push({
        ...d,
        seq: e.seq,
        rule_name: RULE_NAMES[d.rule_id] ?? "unknown",
        status: "open",
        note: null,
        claim: (d.claim_id && claims.get(d.claim_id)) || null,
      });
    } else if (b.kind === "verdict.divergence.updated") {
      const v = out.find((x) => x.divergence_id === b.payload.divergence_id);
      if (v) {
        v.status = b.payload.status;
        v.note = b.payload.note;
      }
    }
  }
  return out;
}

/** Events cited by divergences still standing (not marked a false positive). */
export function evidenceSeqs(divergences: api.DivergenceView[]): Set<number> {
  return new Set(
    divergences.filter((d) => d.status !== "false_positive").flatMap((d) => d.evidence_seqs),
  );
}

const SEVERITY_RANK: Record<api.Severity, number> = { critical: 0, high: 1, medium: 2, low: 3 };

export interface RuleGroup<T> {
  rule_id: string;
  rule_name: string;
  entries: T[];
}

/** The inbox: divergences grouped by rule (in rule order), most severe first. */
export function groupByRule<T extends { divergence: api.DivergenceView }>(
  entries: T[],
): RuleGroup<T>[] {
  const groups = new Map<string, RuleGroup<T>>();
  for (const e of entries) {
    const { rule_id, rule_name } = e.divergence;
    const g = groups.get(rule_id) ?? { rule_id, rule_name, entries: [] };
    g.entries.push(e);
    groups.set(rule_id, g);
  }
  for (const g of groups.values()) {
    g.entries.sort(
      (a, b) => SEVERITY_RANK[a.divergence.severity] - SEVERITY_RANK[b.divergence.severity],
    );
  }
  return [...groups.values()].sort((a, b) => a.rule_id.localeCompare(b.rule_id));
}
