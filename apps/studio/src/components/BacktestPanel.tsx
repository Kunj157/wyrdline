import type * as api from "@wyrdline/schemas/generated-api";
import { Badge, Button } from "@wyrdline/ui";
import { useState } from "react";

import { useBacktest, usePolicies } from "../lib/api";

/**
 * Replay this session against an uploaded policy (`POST /v1/policies`):
 * the findings are listed here and shown on the river while the overlay
 * is on.
 */
export function BacktestPanel({
  sid,
  onReport,
  onSelect,
}: {
  sid: string;
  onReport: (r: api.BacktestReport | null) => void;
  onSelect: (seq: number) => void;
}) {
  const policies = usePolicies(true);
  const backtest = useBacktest(sid);
  const [policy, setPolicy] = useState("");
  const list = policies.data?.policies ?? [];
  const chosen = policy || list[0]?.id || "";
  const report = backtest.data;

  if (policies.data && list.length === 0) {
    return (
      <p className="text-sm text-text-secondary">
        No policies uploaded. Upload a component to the daemon (
        <code className="font-mono">POST /v1/policies?name=…</code>) or use{" "}
        <code className="font-mono">wyrd policy backtest</code>.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex flex-col gap-1 text-sm">
          Policy
          <select
            value={chosen}
            onChange={(e) => setPolicy(e.target.value)}
            className="rounded-sm border border-border-strong bg-surface-1 px-2 py-1 font-mono"
          >
            {list.map((p) => (
              <option key={p.id} value={p.id}>
                {p.id} ({p.version})
              </option>
            ))}
          </select>
        </label>
        <Button
          variant="primary"
          disabled={!chosen || backtest.isPending}
          onClick={() =>
            backtest.mutate(chosen, {
              onSuccess: (r) => onReport(r),
            })
          }
        >
          Backtest
        </Button>
        {report && (
          <Button
            onClick={() => {
              backtest.reset();
              onReport(null);
            }}
          >
            Clear overlay
          </Button>
        )}
      </div>
      {backtest.error && (
        <p role="alert" className="text-sm text-verdict-denied">
          {backtest.error.message}
        </p>
      )}
      {report && (
        <div role="status" className="flex flex-col gap-2 text-sm">
          <p>
            {report.policy_id} would have stopped {report.findings.length} of {report.events} events
            (marked as denied on the river).
          </p>
          <ul className="flex flex-col gap-1">
            {report.findings.map((f) => (
              <li key={`${f.seq}-${f.stage}`}>
                <button
                  type="button"
                  onClick={() => onSelect(Number(f.seq))}
                  className="flex w-full items-start gap-2 rounded-sm px-1 py-0.5 text-left hover:bg-surface-2"
                >
                  <span className="font-mono text-text-muted">#{Number(f.seq)}</span>
                  <Badge
                    meaning={f.decision === "deny" ? "denied" : "irreversible"}
                  >{`${f.stage} ${f.decision}`}</Badge>
                  <span>{f.reason}</span>
                </button>
              </li>
            ))}
          </ul>
          {report.notes.map((n) => (
            <p key={n} className="text-xs text-text-muted">
              {n}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
