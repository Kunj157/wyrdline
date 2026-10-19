import type * as api from "@wyrdline/schemas/generated-api";
import { Badge, Button, type Meaning } from "@wyrdline/ui";
import { type FormEvent, type ReactNode, useId, useState } from "react";

import { useTriage } from "../lib/api";

type Status = api.DivergenceStatus;

const statusMeaning: Record<Status, Meaning> = {
  open: "divergence",
  acknowledged: "ok",
  false_positive: "unknown",
};

const statusLabel: Record<Status, string> = {
  open: "open",
  acknowledged: "acknowledged",
  false_positive: "false positive",
};

/** The triage buttons for each status, in order. */
const actions: { status: Status; label: string }[] = [
  { status: "acknowledged", label: "Acknowledge" },
  { status: "false_positive", label: "False positive" },
  { status: "open", label: "Reopen" },
];

/** Longest note the daemon accepts. */
const MAX_NOTE = 2000;

export interface DivergenceItemProps {
  sid: string;
  divergence: api.DivergenceView;
  /** Select a cited event (the session view); without it evidence is plain text. */
  onSelect?: ((seq: number) => void) | undefined;
  /** Where the divergence was raised (the inbox names the session). */
  context?: ReactNode;
  readOnly: boolean;
}

/**
 * One divergence: what the rule found, the claim and evidence, and its
 * triage. A triage is written to the ledger; while the daemon refuses it
 * (409: the session is recording or sealed) the error is shown and the
 * divergence stays as it was.
 */
export function DivergenceItem({
  sid,
  divergence: d,
  onSelect,
  context,
  readOnly,
}: DivergenceItemProps) {
  const id = useId();
  const triage = useTriage(sid);
  const [target, setTarget] = useState<Status | null>(null);
  const [note, setNote] = useState("");

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!target) return;
    const body = { status: target, note: note.trim() || null };
    triage.mutate(
      { did: d.divergence_id, body },
      {
        onSuccess: () => {
          setTarget(null);
          setNote("");
        },
      },
    );
  };

  return (
    <article
      aria-labelledby={`${id}-title`}
      className="flex flex-col gap-2 rounded-sm border border-border bg-surface-1 p-3 text-sm"
    >
      <header className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <h3 id={`${id}-title`} className="font-medium">
          <span className="font-mono">{d.rule_id}</span> {d.rule_name}
        </h3>
        <Badge meaning={statusMeaning[d.status]}>{statusLabel[d.status]}</Badge>
        <span className="text-xs text-text-secondary">
          {d.severity} · {Math.round(d.confidence_bp / 100)} % confidence
        </span>
        {context}
      </header>
      <p className="break-words">{d.summary}</p>
      {d.claim && (
        <p className="text-text-secondary">
          Claimed in turn {d.claim.turn}: <q className="text-text-primary">{d.claim.quote}</q>
        </p>
      )}
      {d.evidence_seqs.length > 0 && (
        <div className="flex flex-wrap items-center gap-1 text-xs text-text-secondary">
          Evidence
          {d.evidence_seqs.map((seq) =>
            onSelect ? (
              <button
                key={seq}
                type="button"
                aria-label={`Select event #${seq}`}
                onClick={() => onSelect(seq)}
                className="rounded-sm px-1 font-mono text-accent hover:bg-surface-2 hover:underline"
              >
                #{seq}
              </button>
            ) : (
              <span key={seq} className="px-1 font-mono">
                #{seq}
              </span>
            ),
          )}
        </div>
      )}
      {d.note && (
        <p className="text-text-secondary">
          Note: <span className="text-text-primary">{d.note}</span>
        </p>
      )}
      {!readOnly && target === null && (
        <div className="flex flex-wrap gap-2">
          {actions
            .filter((a) => a.status !== d.status)
            .map((a) => (
              <Button key={a.status} size="sm" onClick={() => setTarget(a.status)}>
                {a.label}
              </Button>
            ))}
        </div>
      )}
      {!readOnly && target !== null && (
        <form onSubmit={submit} className="flex flex-col gap-2">
          <div className="flex flex-col gap-1">
            <label htmlFor={`${id}-note`}>Note</label>
            <textarea
              id={`${id}-note`}
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={MAX_NOTE}
              rows={2}
              aria-describedby={`${id}-hint`}
              className="rounded-sm border border-border-strong bg-surface-1 px-2 py-1"
            />
            <span id={`${id}-hint`} className="text-xs text-text-muted">
              Optional. Stored in the ledger, so keep secrets out.
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button type="submit" variant="primary" size="sm" disabled={triage.isPending}>
              Save
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setTarget(null);
                triage.reset();
              }}
            >
              Cancel
            </Button>
            <span className="text-xs text-text-muted">as {statusLabel[target]}</span>
          </div>
        </form>
      )}
      {triage.error && (
        <p role="alert" className="text-verdict-denied">
          {triage.error.message}
        </p>
      )}
    </article>
  );
}
