import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Badge, Card, Kbd } from "@wyrdline/ui";
import { GitFork, Radio } from "lucide-react";

import { useState } from "react";

import { AnimatedNumber } from "../components/AnimatedNumber";
import { setOffline, useCapabilities, useSessions } from "../lib/api";
import { createOfflineClient, loadCorrelator, parseExport } from "../lib/offline";
import { statusMeaning } from "./sessions";

/** Replay an exported session (`wyrd events <session> --json`) without a daemon. */
function OpenExport() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [error, setError] = useState<string | null>(null);
  async function open(file: File) {
    try {
      const events = parseExport(await file.text());
      const client = createOfflineClient(events, file.name, await loadCorrelator());
      setOffline(file.name, client);
      qc.clear();
      const sid = (await client.sessions()).sessions[0]?.id;
      if (sid) await navigate({ to: "/sessions/$sid", params: { sid } });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }
  return (
    <div className="mt-3 text-sm">
      <label className="inline-flex cursor-pointer items-center gap-2 rounded-sm border border-border-strong bg-surface-1 px-3 py-1.5 font-medium hover:bg-surface-2">
        Open a session file
        <input
          type="file"
          accept=".json,.jsonl,application/json"
          className="sr-only"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void open(f);
          }}
        />
      </label>
      <span className="ml-2 text-text-muted">
        from <code className="font-mono">wyrd events &lt;session&gt; --json</code> — replayed
        locally, nothing is uploaded
      </span>
      {error && (
        <p role="alert" className="mt-2 text-verdict-denied">
          {error}
        </p>
      )}
    </div>
  );
}

export function OverviewPage() {
  const sessions = useSessions();
  const caps = useCapabilities();
  const list = sessions.data?.sessions ?? [];
  const live = list.filter((s) => s.status === "recording" || s.status === "degraded");
  const checkpoints = list.reduce((n, s) => n + s.checkpoints.length, 0);
  const forks = list
    .flatMap((s) => s.branches.filter((b) => b.forked_from).map((b) => ({ s, b })))
    .sort((x, y) => y.b.created_ms - x.b.created_ms)
    .slice(0, 5);
  const checks = caps.data?.checks ?? [];
  const ok = checks.filter((c) => c.status === "ok").length;

  return (
    <div className="mx-auto max-w-7xl p-4 md:p-8">
      <p className="font-mono text-xs tracking-[0.2em] text-accent uppercase">Wyrdline Studio</p>
      <h1 className="mt-2 text-3xl font-semibold tracking-tight">Every agent leaves a thread.</h1>
      <p className="mt-2 max-w-2xl text-text-secondary">
        Press <Kbd>Ctrl</Kbd> <Kbd>K</Kbd> to jump to a session, a checkpoint, or an event.
      </p>
      <OpenExport />

      <div className="mt-8 grid auto-rows-[minmax(8rem,auto)] grid-cols-1 gap-4 md:grid-cols-6">
        <Card
          aria-label="Live sessions"
          className="relative overflow-hidden md:col-span-3 md:row-span-2"
        >
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <Radio className="size-4 text-accent" aria-hidden="true" />
            Live sessions
          </h2>
          {live.length === 0 ? (
            <p className="mt-3 text-sm text-text-secondary">
              Nothing is recording. Start a session with{" "}
              <code className="rounded-sm bg-surface-2 px-1.5 py-0.5 font-mono text-xs">
                wyrd run -- &lt;agent&gt;
              </code>
              .
            </p>
          ) : (
            <ul className="mt-3 flex flex-col gap-2">
              {live.map((s) => (
                <li key={s.id} className="wyrd-trail rounded-card p-[1.5px]">
                  <Link
                    to="/sessions/$sid"
                    params={{ sid: s.id }}
                    className="flex items-center justify-between gap-3 rounded-card bg-surface-1 px-3 py-2"
                  >
                    <span className="truncate font-medium">{s.name ?? s.id.slice(0, 10)}</span>
                    <Badge meaning={statusMeaning[s.status] ?? "unknown"}>{s.status}</Badge>
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card aria-label="Sessions recorded" className="md:col-span-1">
          <p className="text-sm text-text-secondary">Sessions</p>
          <AnimatedNumber value={list.length} className="mt-1 block text-3xl font-semibold" />
          <Link to="/sessions" className="mt-2 inline-block text-xs text-accent hover:underline">
            All sessions →
          </Link>
        </Card>
        <Card aria-label="Checkpoints taken" className="md:col-span-1">
          <p className="text-sm text-text-secondary">Checkpoints</p>
          <AnimatedNumber value={checkpoints} className="mt-1 block text-3xl font-semibold" />
        </Card>
        <Card aria-label="Recording now" className="md:col-span-1">
          <p className="text-sm text-text-secondary">Recording now</p>
          <AnimatedNumber value={live.length} className="mt-1 block text-3xl font-semibold" />
        </Card>

        <Card aria-label="Capture health" className="md:col-span-3">
          <h2 className="text-sm font-semibold">Capture health</h2>
          {caps.data ? (
            <>
              <p className="mt-1 text-sm text-text-secondary">
                {ok} of {checks.length} kernel capabilities usable · kernel{" "}
                {caps.data.kernel_release}
              </p>
              <ul className="mt-2 flex flex-wrap gap-1.5">
                {checks.map((c) => (
                  <li key={c.id} title={c.detail}>
                    <Badge
                      meaning={
                        c.status === "ok"
                          ? "ok"
                          : c.status === "missing"
                            ? "denied"
                            : "irreversible"
                      }
                    >
                      {c.id}
                    </Badge>
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p className="mt-1 text-sm text-text-muted">
              {caps.error ? caps.error.message : "Checking…"}
            </p>
          )}
        </Card>

        <Card aria-label="Recent forks and rewinds" className="md:col-span-6">
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <GitFork className="size-4 text-text-secondary" aria-hidden="true" />
            Recent forks and rewinds
          </h2>
          {forks.length === 0 ? (
            <p className="mt-2 text-sm text-text-muted">
              No session has been rewound or forked yet.
            </p>
          ) : (
            <ul className="mt-2 flex flex-col gap-1 text-sm">
              {forks.map(({ s, b }) => (
                <li key={`${s.id}-${b.id}`} className="flex flex-wrap items-baseline gap-2">
                  <Link
                    to="/sessions/$sid"
                    params={{ sid: s.id }}
                    className="font-medium text-accent hover:underline"
                  >
                    {s.name ?? s.id.slice(0, 10)}
                  </Link>
                  <span className="text-text-secondary">
                    branch {b.name} from {b.forked_from}
                  </span>
                  <span className="text-xs text-text-muted">
                    {new Date(b.created_ms).toLocaleString()}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </Card>
      </div>
    </div>
  );
}
