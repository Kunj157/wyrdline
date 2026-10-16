import { Badge } from "@wyrdline/ui";
import { diffLines } from "diff";
import { useState } from "react";

import { useDiff, useDiffFile } from "../lib/api";

const kindMeaning = {
  added: "ok",
  removed: "denied",
  modified: "intent",
  deleted: "denied",
} as const;

/** An overlay whiteout arrives as an added entry of type "deleted". */
const label = (c: { kind: "added" | "removed" | "modified"; entry: string }) =>
  c.entry === "deleted" ? "deleted" : c.kind;

/** Changes between two checkpoints, and a line diff of the chosen file. */
export function DiffView({ sid, a, b }: { sid: string; a: string; b: string }) {
  const changes = useDiff(sid, a, b);
  const [path, setPath] = useState<string | null>(null);
  const file = useDiffFile(sid, a, b, path);
  if (changes.isPending) return <p className="text-sm text-text-muted">Loading changes…</p>;
  if (changes.error) return <p role="alert">{changes.error.message}</p>;
  const list = changes.data?.changes ?? [];
  return (
    <div className="grid gap-3 md:grid-cols-[minmax(12rem,1fr)_2fr]">
      <ul aria-label={`Changes from ${a} to ${b}`} className="flex flex-col gap-1">
        {list.length === 0 && <li className="text-sm text-text-muted">No changes.</li>}
        {list.map((c) => (
          <li key={c.path}>
            <button
              type="button"
              aria-pressed={path === c.path}
              onClick={() => setPath(c.path)}
              className={`flex w-full items-center gap-2 rounded-sm px-2 py-1 text-left text-sm ${path === c.path ? "bg-accent-subtle" : "hover:bg-surface-2"}`}
            >
              <Badge meaning={kindMeaning[label(c)]}>{label(c)}</Badge>
              <span className="truncate font-mono text-xs">{c.path}</span>
            </button>
          </li>
        ))}
      </ul>
      <div className="min-w-0">
        {!path && <p className="text-sm text-text-muted">Choose a file to see its changes.</p>}
        {path && file.isPending && <p className="text-sm text-text-muted">Loading {path}…</p>}
        {file.data && <FileDiffBody before={file.data.before} after={file.data.after} />}
      </div>
    </div>
  );
}

interface Side {
  text: string | null;
  binary: boolean;
  truncated: boolean;
  source: string;
}

function FileDiffBody({ before, after }: { before: Side; after: Side }) {
  if (before.binary || after.binary) {
    return <p className="text-sm">Binary file; no line diff.</p>;
  }
  const parts = diffLines(before.text ?? "", after.text ?? "");
  return (
    <div>
      {(before.truncated || after.truncated) && (
        <p className="mb-1 text-xs text-text-muted">Only the first megabyte is shown.</p>
      )}
      <pre
        aria-label="Line diff"
        className="max-h-[50vh] overflow-auto rounded-sm border border-border bg-surface-1 p-2 font-mono text-xs leading-5"
      >
        {parts.map((p, i) => {
          const sign = p.added ? "+" : p.removed ? "−" : " ";
          const cls = p.added
            ? "bg-vivid-green/15 text-verdict-ok"
            : p.removed
              ? "bg-vivid-red/12 text-verdict-denied"
              : "text-text-secondary";
          return (
            <span key={i} className={`block ${cls}`}>
              {p.value
                .replace(/\n$/, "")
                .split("\n")
                .map((line) => `${sign} ${line}`)
                .join("\n")}
            </span>
          );
        })}
      </pre>
    </div>
  );
}
