import type { Event } from "@wyrdline/schemas";
import type { api } from "@wyrdline/schemas";
import { Badge, Button, Dialog } from "@wyrdline/ui";
import { GitBranch, GitCompare, GitFork, History, Upload } from "lucide-react";
import { useState } from "react";

import { useSessionActions } from "../lib/api";
import { describe, irreversibleAfter } from "../lib/model";
import { DiffView } from "./DiffView";

type Action = { kind: "rewind" | "fork" | "apply" | "diff"; cp: api.CheckpointRecord } | null;

export interface CheckpointPanelProps {
  sid: string;
  checkpoints: api.Checkpoints | undefined;
  events: Event[];
}

/** Checkpoints and branches, with Rewind, Fork, Diff, and Apply. */
export function CheckpointPanel({ sid, checkpoints, events }: CheckpointPanelProps) {
  const [action, setAction] = useState<Action>(null);
  if (!checkpoints) return null;
  const close = () => setAction(null);
  return (
    <section aria-label="Checkpoints and branches">
      <ul className="flex flex-col gap-1">
        {checkpoints.branches.map((b) => (
          <li key={b.id}>
            <p className="flex items-center gap-1.5 text-xs font-medium text-text-secondary">
              <GitBranch className="size-3.5" aria-hidden="true" />
              branch {b.name}
              {b.id === checkpoints.current_branch && <Badge meaning="ok">current</Badge>}
              {b.forked_from && <span className="text-text-muted">from {b.forked_from}</span>}
            </p>
            <ul className="mt-1 ml-4 flex flex-col gap-1 border-l border-border pl-3">
              {checkpoints.checkpoints
                .filter((c) => c.branch === b.id)
                .map((c) => (
                  <li key={c.id} className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-mono">{c.id}</span>
                    <span className="text-xs text-text-muted">
                      seq {c.seq} · {c.files} files
                    </span>
                    <span className="ml-auto flex gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={<History className="size-3.5" aria-hidden="true" />}
                        onClick={() => setAction({ kind: "rewind", cp: c })}
                        aria-label={`Rewind to ${c.id}`}
                      >
                        Rewind
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={<GitFork className="size-3.5" aria-hidden="true" />}
                        onClick={() => setAction({ kind: "fork", cp: c })}
                        aria-label={`Fork from ${c.id}`}
                      >
                        Fork
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={<GitCompare className="size-3.5" aria-hidden="true" />}
                        onClick={() => setAction({ kind: "diff", cp: c })}
                        aria-label={`Diff ${c.id}`}
                      >
                        Diff
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={<Upload className="size-3.5" aria-hidden="true" />}
                        onClick={() => setAction({ kind: "apply", cp: c })}
                        aria-label={`Apply ${c.id}`}
                      >
                        Apply
                      </Button>
                    </span>
                  </li>
                ))}
            </ul>
          </li>
        ))}
      </ul>
      {action?.kind === "rewind" && (
        <RewindDialog sid={sid} cp={action.cp} events={events} onClose={close} />
      )}
      {action?.kind === "fork" && <ForkDialog sid={sid} cp={action.cp} onClose={close} />}
      {action?.kind === "apply" && <ApplyDialog sid={sid} cp={action.cp} onClose={close} />}
      {action?.kind === "diff" && (
        <DiffDialog
          sid={sid}
          cp={action.cp}
          checkpoints={checkpoints.checkpoints}
          onClose={close}
        />
      )}
    </section>
  );
}

function Irreversible({ events, seq }: { events: Event[]; seq: number }) {
  const left = irreversibleAfter(events, seq);
  if (left.length === 0) {
    return <p className="text-sm">Every recorded effect after this checkpoint can be undone.</p>;
  }
  return (
    <div>
      <p className="mb-1 flex items-center gap-1.5 text-sm font-medium">
        <Badge meaning="irreversible">irreversible</Badge>
        {left.length} effect{left.length === 1 ? "" : "s"} after this checkpoint cannot be undone:
      </p>
      <ul className="max-h-40 overflow-auto rounded-sm bg-surface-2 p-2 font-mono text-xs">
        {left.map((e) => (
          <li key={e.seq}>
            #{e.seq} {describe(e)}
          </li>
        ))}
      </ul>
    </div>
  );
}

function RewindDialog({
  sid,
  cp,
  events,
  onClose,
}: {
  sid: string;
  cp: api.CheckpointRecord;
  events: Event[];
  onClose: () => void;
}) {
  const { rewind } = useSessionActions(sid);
  const [relaunch, setRelaunch] = useState(false);
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Rewind to ${cp.id}`}
      description="The workspace inside the Cell returns to this checkpoint on a new branch. The current branch is kept."
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            disabled={rewind.isPending || rewind.isSuccess}
            onClick={() => rewind.mutate({ target: cp.id, relaunch })}
          >
            Rewind
          </Button>
        </>
      }
    >
      <Irreversible events={events} seq={cp.seq} />
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={relaunch} onChange={(e) => setRelaunch(e.target.checked)} />
        Start the agent again on the new branch
      </label>
      {rewind.isSuccess && (
        <p role="status">
          Rewound:{" "}
          {"branch" in rewind.data ? `branch ${rewind.data.branch.name}` : "relaunching the agent"}.
        </p>
      )}
      {rewind.error && (
        <p role="alert" className="text-verdict-denied">
          {rewind.error.message}
        </p>
      )}
    </Dialog>
  );
}

function ForkDialog({
  sid,
  cp,
  onClose,
}: {
  sid: string;
  cp: api.CheckpointRecord;
  onClose: () => void;
}) {
  const { fork } = useSessionActions(sid);
  const [name, setName] = useState("");
  const valid = /^[A-Za-z0-9_-]{1,64}$/.test(name);
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Fork from ${cp.id}`}
      description="Start a named branch from this checkpoint."
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!valid || fork.isPending || fork.isSuccess}
            onClick={() => fork.mutate({ target: cp.id, name })}
          >
            Fork
          </Button>
        </>
      }
    >
      <label className="flex flex-col gap-1 text-sm">
        Branch name
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          aria-invalid={name !== "" && !valid}
          className="rounded-sm border border-border-strong bg-surface-1 px-2 py-1 font-mono"
          placeholder="careful-retry"
        />
      </label>
      {name !== "" && !valid && (
        <p className="text-xs text-verdict-denied">Letters, digits, - and _ only.</p>
      )}
      {fork.isSuccess && <p role="status">Forked branch {name}.</p>}
      {fork.error && (
        <p role="alert" className="text-verdict-denied">
          {fork.error.message}
        </p>
      )}
    </Dialog>
  );
}

function ApplyDialog({
  sid,
  cp,
  onClose,
}: {
  sid: string;
  cp: api.CheckpointRecord;
  onClose: () => void;
}) {
  const { apply } = useSessionActions(sid);
  const preview = apply.data && !apply.data.applied ? apply.data : null;
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Apply ${cp.id} to the real workspace`}
      description="Writes this checkpoint's changes into your working tree. Preview first."
      footer={
        <>
          <Button onClick={onClose}>Close</Button>
          <Button
            disabled={apply.isPending}
            onClick={() => apply.mutate({ target: cp.id, dry_run: true, force: false })}
          >
            Preview
          </Button>
          <Button
            variant="primary"
            disabled={apply.isPending || !preview || apply.data?.applied}
            onClick={() => apply.mutate({ target: cp.id, dry_run: false, force: false })}
          >
            Apply
          </Button>
        </>
      }
    >
      {preview && (
        <ul
          aria-label="Planned changes"
          className="max-h-48 overflow-auto rounded-sm bg-surface-2 p-2 font-mono text-xs"
        >
          {preview.ops.map((op, i) => (
            <li key={i}>
              {op.op} {op.path}
            </li>
          ))}
        </ul>
      )}
      {apply.data?.applied && (
        <p role="status">Applied {apply.data.ops.length} changes to the workspace.</p>
      )}
      {apply.error && (
        <p role="alert" className="text-verdict-denied">
          {apply.error.message}
        </p>
      )}
    </Dialog>
  );
}

function DiffDialog({
  sid,
  cp,
  checkpoints,
  onClose,
}: {
  sid: string;
  cp: api.CheckpointRecord;
  checkpoints: api.CheckpointRecord[];
  onClose: () => void;
}) {
  const others = checkpoints.filter((c) => c.id !== cp.id);
  const [other, setOther] = useState(cp.parent ?? others[0]?.id ?? cp.id);
  return (
    <Dialog
      open
      onClose={onClose}
      title={`Diff ${other} → ${cp.id}`}
      className="w-[min(64rem,calc(100vw-2rem))]"
    >
      <label className="flex items-center gap-2 text-sm">
        Compare with
        <select
          value={other}
          onChange={(e) => setOther(e.target.value)}
          className="rounded-sm border border-border-strong bg-surface-1 px-2 py-1 font-mono"
        >
          {others.map((c) => (
            <option key={c.id} value={c.id}>
              {c.id}
            </option>
          ))}
        </select>
      </label>
      <DiffView sid={sid} a={other} b={cp.id} />
    </Dialog>
  );
}
