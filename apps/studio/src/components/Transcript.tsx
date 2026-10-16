import type { Event } from "@wyrdline/schemas";

import { transcript } from "../lib/model";

/** The redacted conversation, oldest first. */
export function Transcript({
  events,
  onSelect,
}: {
  events: Event[];
  onSelect: (seq: number) => void;
}) {
  const turns = transcript(events);
  if (turns.length === 0) {
    return <p className="text-sm text-text-muted">No transcript recorded for this session.</p>;
  }
  return (
    <ol className="flex max-h-[60vh] flex-col gap-2 overflow-auto">
      {turns.map((t) => (
        <li key={t.seq}>
          <button
            type="button"
            onClick={() => onSelect(t.seq)}
            className={`w-full rounded-card border border-border p-3 text-left text-sm ${t.role === "user" ? "bg-surface-2" : "bg-surface-1"}`}
          >
            <span className="mb-1 block text-xs font-medium tracking-wide text-text-muted uppercase">
              {t.role} · turn {t.turn}
            </span>
            <span className="whitespace-pre-wrap">{t.text}</span>
          </button>
        </li>
      ))}
    </ol>
  );
}
