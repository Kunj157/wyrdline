import { Button, Dialog } from "@wyrdline/ui";
import { ShieldQuestion } from "lucide-react";

import { useAnswerAsk, useAsks } from "../lib/api";

/**
 * The oldest question the running session's policy is waiting on. The
 * tool call stays blocked until it is answered here, with `wyrd ask`, or
 * by the timeout (which denies it).
 */
export function AskDialog({ sid, live }: { sid: string; live: boolean }) {
  const asks = useAsks(sid, live);
  const answer = useAnswerAsk(sid);
  const ask = asks.data?.asks[0];
  if (!ask) return null;
  const decide = (allow: boolean) => answer.mutate({ id: Number(ask.id), allow });
  const seconds = Math.max(0, Math.round(Number(ask.expires_in_ms) / 1000));
  return (
    <Dialog
      open
      onClose={() => decide(false)}
      title={`Allow ${ask.tool}?`}
      description={`The policy ${ask.policy_id} asks before this call runs. Without an answer it is denied in ${seconds} s.`}
      footer={
        <>
          <Button onClick={() => decide(false)} disabled={answer.isPending}>
            Deny
          </Button>
          <Button variant="primary" onClick={() => decide(true)} disabled={answer.isPending}>
            Allow
          </Button>
        </>
      }
    >
      <p className="flex items-start gap-2 text-sm text-verdict-irreversible">
        <ShieldQuestion className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <span>{ask.reason}</span>
      </p>
      <pre
        tabIndex={0}
        className="max-h-48 overflow-auto rounded-sm bg-surface-2 p-2 font-mono text-xs text-text-secondary"
      >
        {ask.args}
      </pre>
      {(asks.data?.asks.length ?? 0) > 1 && (
        <p className="text-xs text-text-muted">{(asks.data?.asks.length ?? 1) - 1} more waiting</p>
      )}
      {answer.error && (
        <p role="alert" className="text-sm text-verdict-denied">
          {answer.error.message}
        </p>
      )}
    </Dialog>
  );
}
