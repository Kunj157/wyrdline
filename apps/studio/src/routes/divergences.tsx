import { Link } from "@tanstack/react-router";
import type { api } from "@wyrdline/schemas";
import { cx } from "@wyrdline/ui";
import { useId, useState } from "react";

import { DivergenceItem } from "../components/DivergenceItem";
import { offlineSource, useAllDivergences, useSessions } from "../lib/api";
import { groupByRule, type RuleGroup } from "../lib/divergences";

type Entry = { session: api.SessionRecord; divergence: api.DivergenceView };
type Filter = "open" | "all";

function Group({ group, readOnly }: { group: RuleGroup<Entry>; readOnly: boolean }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <h2 id={id} className="flex items-baseline gap-2 text-base font-semibold">
        <span className="font-mono">{group.rule_id}</span> {group.rule_name}
        <span className="text-sm font-normal text-text-muted">{group.entries.length}</span>
      </h2>
      {group.entries.map(({ session, divergence }) => (
        <DivergenceItem
          key={`${session.id}-${divergence.divergence_id}`}
          sid={session.id}
          divergence={divergence}
          readOnly={readOnly}
          title={
            <Link
              to="/sessions/$sid"
              params={{ sid: session.id }}
              className="text-accent hover:underline"
            >
              {session.name ?? session.id.slice(0, 10)}
            </Link>
          }
        />
      ))}
    </section>
  );
}

/**
 * Divergences across every session, grouped by rule, for triage
 * (plan §16.3). Composed from the sessions list and each session's
 * divergences.
 */
export function DivergencesPage() {
  const sessions = useSessions();
  const list = sessions.data?.sessions ?? [];
  const results = useAllDivergences(list);
  const [filter, setFilter] = useState<Filter>("open");
  const entries: Entry[] = list.flatMap((session, i) =>
    (results[i]?.data?.divergences ?? []).map((divergence) => ({ session, divergence })),
  );
  const open = entries.filter((e) => e.divergence.status === "open");
  const shown = filter === "open" ? open : entries;
  const groups = groupByRule(shown);
  const failed = results.filter((r) => r.error);
  const loading = sessions.isPending || results.some((r) => r.isPending);
  const openSessions = new Set(open.map((e) => e.session.id)).size;

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 p-4 md:p-8">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Divergences</h1>
          <p className="mt-1 text-sm text-text-secondary">
            {loading
              ? "Loading divergences…"
              : `${open.length} open across ${openSessions} ${openSessions === 1 ? "session" : "sessions"} · grouped by rule`}
          </p>
        </div>
        <div role="group" aria-label="Show" className="flex rounded-sm border border-border-strong">
          {(["open", "all"] as const).map((f) => (
            <button
              key={f}
              type="button"
              aria-pressed={filter === f}
              onClick={() => setFilter(f)}
              className={cx(
                "px-3 py-1 text-sm first:rounded-l-sm last:rounded-r-sm",
                filter === f
                  ? "bg-accent text-on-accent"
                  : "bg-surface-1 text-text-secondary hover:bg-surface-2",
              )}
            >
              {f === "open" ? "Open" : "All"}
            </button>
          ))}
        </div>
      </header>
      {sessions.error && (
        <p role="alert" className="text-verdict-denied">
          {sessions.error.message}
        </p>
      )}
      {failed.length > 0 && (
        <p role="alert" className="text-sm text-verdict-denied">
          Divergences of {failed.length} {failed.length === 1 ? "session" : "sessions"} could not be
          loaded: {failed[0]?.error?.message}
        </p>
      )}
      {!loading && groups.length === 0 && (
        <p className="text-text-secondary">
          {filter === "open" ? "No open divergences." : "No divergences recorded yet."}
        </p>
      )}
      {groups.map((g) => (
        <Group key={g.rule_id} group={g} readOnly={offlineSource() !== null} />
      ))}
    </div>
  );
}
