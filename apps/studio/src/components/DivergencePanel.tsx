import { offlineSource, useDivergences } from "../lib/api";
import { DivergenceItem } from "./DivergenceItem";

/**
 * The session's divergences in ledger order. Evidence selects the cited
 * event in the river, the table, and the inspector.
 */
export function DivergencePanel({
  sid,
  onSelect,
}: {
  sid: string;
  onSelect: (seq: number) => void;
}) {
  const divergences = useDivergences(sid);
  const readOnly = offlineSource() !== null;
  if (divergences.error) {
    return (
      <p role="alert" className="text-sm text-verdict-denied">
        {divergences.error.message}
      </p>
    );
  }
  if (!divergences.data) return <p className="text-sm text-text-muted">Loading divergences…</p>;
  const list = divergences.data.divergences;
  if (list.length === 0) {
    return (
      <p className="text-sm text-text-secondary">
        No divergences: nothing the agent said or did broke a rule.
      </p>
    );
  }
  const open = list.filter((d) => d.status === "open").length;
  return (
    <div className="flex flex-col gap-2">
      <p className="text-sm text-text-secondary">
        {list.length} {list.length === 1 ? "divergence" : "divergences"}, {open} open
        {readOnly && " · triage needs the daemon"}
      </p>
      {list.map((d) => (
        <DivergenceItem
          key={d.divergence_id}
          sid={sid}
          divergence={d}
          onSelect={onSelect}
          readOnly={readOnly}
        />
      ))}
    </div>
  );
}
