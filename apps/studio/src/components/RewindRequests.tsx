import type { api } from "@wyrdline/schemas";
import { Badge, Button, Dialog } from "@wyrdline/ui";
import { Bot, History, X } from "lucide-react";
import { useState } from "react";

import { useDecideRewind, useRewindRequests } from "../lib/api";

/**
 * Rewind requests the agent made through `wyrd mcp`. The agent can only
 * ask: nothing is rewound until the user confirms here (or with
 * `wyrd rewind --confirm`).
 */
export function RewindRequests({ sid, live }: { sid: string; live: boolean }) {
  const requests = useRewindRequests(sid, live);
  const decide = useDecideRewind(sid);
  const [confirming, setConfirming] = useState<api.RewindRequest | null>(null);
  const pending = requests.data?.requests.filter((r) => r.status === "pending") ?? [];
  if (pending.length === 0 && !decide.isSuccess) return null;
  return (
    <section aria-label="Rewind requests" className="mb-3 flex flex-col gap-2">
      {pending.map((r) => (
        <div
          key={r.id}
          className="flex flex-wrap items-start gap-2 rounded-sm border border-border bg-surface-2 p-2 text-sm"
        >
          <Bot className="mt-0.5 size-4 shrink-0 text-text-muted" aria-hidden="true" />
          <div className="min-w-0 flex-1">
            <p className="flex flex-wrap items-center gap-1.5">
              The agent asks to rewind to <span className="font-mono">{r.checkpoint_id}</span>
              <Badge meaning="irreversible">needs you</Badge>
            </p>
            <p className="mt-1 break-words text-text-secondary">{r.reason}</p>
          </div>
          <span className="flex gap-1">
            <Button
              size="sm"
              icon={<X className="size-3.5" aria-hidden="true" />}
              disabled={decide.isPending}
              onClick={() => decide.mutate({ id: Number(r.id), confirm: false })}
              aria-label={`Reject rewind request ${r.id}`}
            >
              Reject
            </Button>
            <Button
              size="sm"
              variant="primary"
              icon={<History className="size-3.5" aria-hidden="true" />}
              disabled={decide.isPending}
              onClick={() => setConfirming(r)}
              aria-label={`Review rewind request ${r.id}`}
            >
              Review
            </Button>
          </span>
        </div>
      ))}
      {decide.isSuccess && (
        <p role="status" className="text-xs text-text-muted">
          {decide.data.branch
            ? `Rewound to ${decide.data.request.checkpoint_id}: now on branch ${decide.data.branch.name}.`
            : `Rejected the request to rewind to ${decide.data.request.checkpoint_id}; nothing was rewound.`}
        </p>
      )}
      {decide.error && !confirming && (
        <p role="alert" className="text-sm text-verdict-denied">
          {decide.error.message}
        </p>
      )}
      {confirming && (
        <Dialog
          open
          onClose={() => setConfirming(null)}
          title={`Rewind to ${confirming.checkpoint_id}?`}
          description="The agent asked for this rewind. Confirming stops the agent if it runs and returns the workspace inside the Cell to this checkpoint on a new branch. The current branch is kept."
          footer={
            <>
              <Button onClick={() => setConfirming(null)}>Cancel</Button>
              <Button
                variant="primary"
                disabled={decide.isPending}
                onClick={() =>
                  decide.mutate(
                    { id: Number(confirming.id), confirm: true },
                    { onSuccess: () => setConfirming(null) },
                  )
                }
              >
                Confirm rewind
              </Button>
            </>
          }
        >
          <p className="text-sm">
            <span className="text-text-muted">Reason given: </span>
            <span className="break-words">{confirming.reason}</span>
          </p>
          {decide.error && (
            <p role="alert" className="text-sm text-verdict-denied">
              {decide.error.message}
            </p>
          )}
        </Dialog>
      )}
    </section>
  );
}
