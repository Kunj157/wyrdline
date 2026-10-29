import type * as api from "@wyrdline/schemas/generated-api";

/**
 * Read a backtest report produced elsewhere (`wyrd policy backtest
 * <policy.wasm> <session|bundle> --json`), for the overlay in offline
 * mode, where no policy host is available. The report must belong to the
 * open session: same session id and event count, every finding a seq of
 * this session. Untrusted input: only the known fields are kept.
 */
export function parseBacktestReport(
  text: string,
  session: { id: string; events: number },
): api.BacktestReport {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw new Error("not JSON: expected the output of `wyrd policy backtest … --json`");
  }
  const r = raw as Record<string, unknown>;
  const str = (v: unknown) => typeof v === "string";
  if (
    !r ||
    typeof r !== "object" ||
    !str(r.policy_id) ||
    !str(r.policy_version) ||
    !str(r.session_id) ||
    typeof r.events !== "number" ||
    !Array.isArray(r.findings) ||
    !Array.isArray(r.notes)
  ) {
    throw new Error("not a backtest report (expected `wyrd policy backtest … --json`)");
  }
  if (r.session_id !== session.id || r.events !== session.events) {
    throw new Error(
      `the report is for session ${String(r.session_id)} with ${String(r.events)} events, ` +
        `not this one (${session.id}, ${session.events} events)`,
    );
  }
  const findings: api.BacktestFinding[] = r.findings.map((f: unknown, i: number) => {
    const x = f as Record<string, unknown>;
    if (
      !x ||
      typeof x.seq !== "number" ||
      !Number.isInteger(x.seq) ||
      x.seq < 1 ||
      x.seq > session.events ||
      !str(x.stage) ||
      !str(x.decision) ||
      !str(x.reason)
    ) {
      throw new Error(`finding ${i + 1} is invalid or names a seq outside this session`);
    }
    return {
      seq: x.seq,
      stage: x.stage as string,
      decision: x.decision as string,
      reason: x.reason as string,
    };
  });
  return {
    policy_id: r.policy_id as string,
    policy_version: r.policy_version as string,
    session_id: r.session_id as string,
    events: r.events,
    findings,
    notes: r.notes.filter(str) as string[],
  };
}
