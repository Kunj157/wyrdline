import type { Event } from "@wyrdline/schemas";
import type { api } from "@wyrdline/schemas";
import { Badge, Tabs } from "@wyrdline/ui";
import { useState } from "react";

import { describe, isIrreversible, meaningOf, planeOf } from "../lib/model";

export interface InspectorProps {
  event: Event | undefined;
  graph: api.Graph | undefined;
  events: Event[];
  /** The session's divergences; those citing the event are listed. */
  divergences?: api.DivergenceView[] | undefined;
}

const pct = (bp: number) => `${Math.round(bp / 100)} %`;

/**
 * The selected event: what it is, which call caused it, which guard ruled,
 * and which divergences cite it.
 */
export function Inspector({ event, graph, events, divergences = [] }: InspectorProps) {
  const [tab, setTab] = useState("event");
  if (!event) {
    return <p className="text-sm text-text-muted">Select an event in the river or the table.</p>;
  }
  const effect = graph?.effects.find((f) => f.seq === event.seq);
  const call =
    (effect?.call_id && graph?.calls.find((c) => c.call_id === effect.call_id)) ||
    graph?.calls.find((c) => c.started_seq === event.seq);
  const decisions = events.filter(
    (e) =>
      e.body.kind === "guard.decision" &&
      (e.body.payload.subject_seq === event.seq ||
        (call && e.body.payload.call_id === call.call_id)),
  );
  const citing = divergences.filter(
    (d) => d.seq === event.seq || d.evidence_seqs.includes(event.seq),
  );
  return (
    <section aria-label="Inspector">
      <header className="mb-2 flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs text-text-muted">#{event.seq}</span>
        <Badge meaning={meaningOf(event)}>{planeOf(event)}</Badge>
        {isIrreversible(event) && <Badge meaning="irreversible">irreversible</Badge>}
      </header>
      <p className="mb-3 text-sm font-medium break-words">{describe(event)}</p>
      <Tabs
        label="Inspector"
        value={tab}
        onChange={setTab}
        tabs={[
          {
            id: "event",
            label: "Event",
            panel: (
              <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 text-sm">
                <dt className="text-text-muted">kind</dt>
                <dd className="font-mono text-xs">{event.body.kind}</dd>
                <dt className="text-text-muted">time</dt>
                <dd>{new Date(event.ts_wall_ms).toLocaleTimeString()}</dd>
                <dt className="text-text-muted">process</dt>
                <dd className="font-mono text-xs">
                  {event.process ? `pid ${event.process.pid} (parent ${event.process.ppid})` : "—"}
                </dd>
                {event.drops_since_last > 0 && (
                  <>
                    <dt className="text-text-muted">drops</dt>
                    <dd>
                      <Badge meaning="divergence">{`${event.drops_since_last} lost before this`}</Badge>
                    </dd>
                  </>
                )}
                <dt className="text-text-muted">payload</dt>
                <dd>
                  <pre className="max-h-48 overflow-auto rounded-sm bg-surface-2 p-2 font-mono text-xs whitespace-pre-wrap break-all">
                    {JSON.stringify(event.body.payload, null, 2)}
                  </pre>
                </dd>
              </dl>
            ),
          },
          {
            id: "attribution",
            label: "Attribution",
            panel: effect ? (
              <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 text-sm">
                <dt className="text-text-muted">cause</dt>
                <dd>{effect.attribution}</dd>
                <dt className="text-text-muted">confidence</dt>
                <dd>{pct(effect.confidence_bp)}</dd>
                <dt className="text-text-muted">tool call</dt>
                <dd className="font-mono text-xs">
                  {call
                    ? `${call.call_id} · ${call.tool} ${call.command ?? call.target_path ?? ""}`
                    : effect.candidates.length > 0
                      ? `one of ${effect.candidates.join(", ")}`
                      : "none"}
                </dd>
              </dl>
            ) : call ? (
              <p className="text-sm">
                Tool call <span className="font-mono">{call.call_id}</span> caused{" "}
                {call.effects.length} effect
                {call.effects.length === 1 ? "" : "s"}.
              </p>
            ) : (
              <p className="text-sm text-text-muted">Not a kernel effect or tool call.</p>
            ),
          },
          {
            id: "policy",
            label: "Policy",
            panel:
              decisions.length === 0 ? (
                <p className="text-sm text-text-muted">
                  No policy decision recorded for this event.
                </p>
              ) : (
                <ul className="flex flex-col gap-1 text-sm">
                  {decisions.map((d) => (
                    <li key={d.seq} className="flex items-center gap-2">
                      <Badge meaning={meaningOf(d)}>
                        {d.body.kind === "guard.decision" ? d.body.payload.decision : "decision"}
                      </Badge>
                      {describe(d)}
                    </li>
                  ))}
                </ul>
              ),
          },
          {
            id: "divergences",
            label: citing.length > 0 ? `Divergences (${citing.length})` : "Divergences",
            panel:
              citing.length === 0 ? (
                <p className="text-sm text-text-muted">No divergence cites this event.</p>
              ) : (
                <ul className="flex flex-col gap-1 text-sm">
                  {citing.map((d) => (
                    <li key={d.divergence_id} className="flex flex-wrap items-center gap-2">
                      <Badge meaning="divergence">{d.rule_id}</Badge>
                      <span className="font-medium">{d.rule_name}</span>
                      <span className="text-xs text-text-secondary">
                        {d.severity} · {d.status.replace("_", " ")}
                      </span>
                      <span className="w-full break-words text-text-secondary">{d.summary}</span>
                    </li>
                  ))}
                </ul>
              ),
          },
        ]}
      />
    </section>
  );
}
