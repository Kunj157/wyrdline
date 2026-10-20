import type * as api from "@wyrdline/schemas/generated-api";
import { Button } from "@wyrdline/ui";
import { type FormEvent, type ReactNode, useId, useState } from "react";

import {
  addComment,
  deleteComment,
  LIMITS,
  pinCheckpoint,
  resolveComment,
  setTriage,
  TRIAGE_STATES,
  type TriageState,
  unpinCheckpoint,
} from "../lib/annotations/doc";
import { useAnnotations } from "../lib/annotations/use-annotations";

const NAME_KEY = "wyrd.annotator";

function storedName(): string {
  try {
    return localStorage.getItem(NAME_KEY) ?? "";
  } catch {
    return "";
  }
}

const triageLabel: Record<TriageState, string> = {
  open: "open",
  acknowledged: "acknowledged",
  false_positive: "false positive",
};

const field = "rounded-sm border border-border-strong bg-surface-1 px-2 py-1 text-sm";

export interface AnnotationsPanelProps {
  sid: string;
  /** The selected event: new comments are anchored to it. */
  selected: number | null;
  onSelect: (seq: number) => void;
  divergences: api.DivergenceView[];
  checkpoints: api.CheckpointRecord[];
  /** Where the annotations are kept or synced (shown above the list). */
  status?: ReactNode;
  readOnly?: boolean;
}

/**
 * Team annotations of a session: comments anchored to seq ranges, the
 * team's triage of each divergence and pinned checkpoints. Edits go to the
 * local Automerge replica first and reach teammates through sync.
 */
export function AnnotationsPanel({
  sid,
  selected,
  onSelect,
  divergences,
  checkpoints,
  status,
  readOnly = false,
}: AnnotationsPanelProps) {
  const id = useId();
  const { replica, snapshot, error } = useAnnotations(sid);
  const view = snapshot?.view;
  const [name, setName] = useState(storedName);
  const initial = selected === null ? "" : String(selected);
  const [from, setFrom] = useState(initial);
  const [to, setTo] = useState(initial);
  const [text, setText] = useState("");
  const [problem, setProblem] = useState<string | null>(null);

  // A newly selected event becomes the anchor of the next comment.
  const [anchor, setAnchor] = useState(selected);
  if (selected !== anchor) {
    setAnchor(selected);
    if (selected !== null) {
      setFrom(String(selected));
      setTo(String(selected));
    }
  }

  if (error) {
    return (
      <p role="alert" className="text-sm text-verdict-denied">
        Annotations are unavailable: {error}
      </p>
    );
  }
  if (!replica || !view) return <p className="text-sm text-text-muted">Loading annotations…</p>;

  const author = name.trim() || "anonymous";
  const edit = (f: Parameters<typeof replica.change>[0]) => {
    try {
      replica.change(f);
      setProblem(null);
      return true;
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
      return false;
    }
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const range = { from: Number(from), to: Number(to || from) };
    if (edit((d) => addComment(d, { ...range, text, author }))) setText("");
  };
  const rememberName = (v: string) => {
    setName(v);
    try {
      localStorage.setItem(NAME_KEY, v);
    } catch {
      // Private windows may refuse storage; the name then lasts this visit.
    }
  };

  return (
    <section aria-label="Annotations" className="flex flex-col gap-4 text-sm">
      <p role="status" className="text-xs text-text-muted">
        {snapshot?.saved ? "Saved on this device" : "Saving…"}
        {status}
      </p>
      {problem && (
        <p role="alert" className="text-verdict-denied">
          {problem}
        </p>
      )}

      <section aria-labelledby={`${id}-comments`} className="flex flex-col gap-2">
        <h3 id={`${id}-comments`} className="font-semibold">
          Comments ({view.comments.length})
        </h3>
        {view.comments.length === 0 && (
          <p className="text-text-secondary">No comments yet. Select an event and add one.</p>
        )}
        <ul className="flex flex-col gap-2">
          {view.comments.map((c) => (
            <li key={c.id}>
              <article
                aria-label={`Comment by ${c.author} on seq ${c.from}`}
                className="rounded-sm border border-border bg-surface-1 p-2"
              >
                <header className="flex flex-wrap items-center gap-2 text-xs text-text-muted">
                  <button
                    type="button"
                    className="font-mono underline"
                    onClick={() => onSelect(c.from)}
                  >
                    {c.from === c.to ? `seq ${c.from}` : `seq ${c.from}–${c.to}`}
                  </button>
                  <span>{c.author}</span>
                  <time dateTime={new Date(c.at).toISOString()}>
                    {new Date(c.at).toLocaleString()}
                  </time>
                  {c.resolved && <span className="text-verdict-ok">resolved</span>}
                </header>
                <p className="mt-1 whitespace-pre-wrap break-words">{c.text}</p>
                {!readOnly && (
                  <div className="mt-1 flex gap-2">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => edit((d) => resolveComment(d, c.id, !c.resolved))}
                    >
                      {c.resolved ? "Reopen" : "Resolve"}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => edit((d) => deleteComment(d, c.id))}
                    >
                      Delete
                    </Button>
                  </div>
                )}
              </article>
            </li>
          ))}
        </ul>
        {!readOnly && (
          <form onSubmit={submit} aria-label="Add a comment" className="flex flex-col gap-2">
            <div className="flex flex-wrap items-end gap-2">
              <label className="flex flex-col gap-1">
                Your name
                <input
                  className={field}
                  value={name}
                  maxLength={LIMITS.author}
                  onChange={(e) => rememberName(e.target.value)}
                />
              </label>
              <label className="flex flex-col gap-1">
                From seq
                <input
                  className={`${field} w-24`}
                  inputMode="numeric"
                  required
                  value={from}
                  onChange={(e) => setFrom(e.target.value)}
                />
              </label>
              <label className="flex flex-col gap-1">
                To seq
                <input
                  className={`${field} w-24`}
                  inputMode="numeric"
                  value={to}
                  onChange={(e) => setTo(e.target.value)}
                />
              </label>
            </div>
            <label className="flex flex-col gap-1">
              Comment
              <textarea
                className={field}
                rows={2}
                required
                maxLength={LIMITS.text}
                value={text}
                onChange={(e) => setText(e.target.value)}
              />
            </label>
            <div>
              <Button type="submit">Add comment</Button>
            </div>
          </form>
        )}
      </section>

      {divergences.length > 0 && (
        <section aria-labelledby={`${id}-triage`} className="flex flex-col gap-2">
          <h3 id={`${id}-triage`} className="font-semibold">
            Team triage
          </h3>
          <ul className="flex flex-col gap-1">
            {divergences.map((d) => {
              const t = view.triage.get(d.divergence_id);
              return (
                <li key={d.divergence_id} className="flex flex-wrap items-center gap-2">
                  <label className="flex items-center gap-2">
                    <span className="font-mono">{d.rule_id}</span>
                    <span className="text-text-secondary">{d.rule_name}</span>
                    <select
                      className={field}
                      disabled={readOnly}
                      value={t?.state ?? "open"}
                      onChange={(e) =>
                        edit((doc) =>
                          setTriage(doc, d.divergence_id, e.target.value as TriageState, author),
                        )
                      }
                    >
                      {TRIAGE_STATES.map((s) => (
                        <option key={s} value={s}>
                          {triageLabel[s]}
                        </option>
                      ))}
                    </select>
                  </label>
                  {t && <span className="text-xs text-text-muted">by {t.by}</span>}
                </li>
              );
            })}
          </ul>
        </section>
      )}

      {checkpoints.length > 0 && (
        <section aria-labelledby={`${id}-pins`} className="flex flex-col gap-2">
          <h3 id={`${id}-pins`} className="font-semibold">
            Pinned checkpoints
          </h3>
          <ul className="flex flex-col gap-1">
            {checkpoints.map((c) => {
              const pin = view.pins.get(c.id);
              const label = `checkpoint at seq ${c.seq}`;
              return (
                <li key={c.id} className="flex flex-wrap items-center gap-2">
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      disabled={readOnly}
                      checked={Boolean(pin)}
                      onChange={(e) =>
                        edit((d) =>
                          e.target.checked
                            ? pinCheckpoint(d, c.id, label, author)
                            : unpinCheckpoint(d, c.id),
                        )
                      }
                    />
                    Pin {label}
                  </label>
                  {pin && <span className="text-xs text-text-muted">pinned by {pin.by}</span>}
                </li>
              );
            })}
          </ul>
        </section>
      )}
    </section>
  );
}
