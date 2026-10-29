import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { Card } from "@wyrdline/ui";
import { type FormEvent, useId, useState } from "react";

import {
  type HubMe,
  HubError,
  loginUrl,
  percent,
  share,
  useHubMe,
  useHubSearch,
  useHubStats,
} from "../lib/hub";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export type HubSearchParams = { org?: string; q?: string };

export function validateHubSearch(s: Record<string, unknown>): HubSearchParams {
  return {
    org: typeof s.org === "string" ? s.org : undefined,
    q: typeof s.q === "string" ? s.q : undefined,
  };
}

/** Signed in with an org picked, or what to do about it. */
function useOrg(page: "/hub" | "/hub/search"): {
  me: HubMe | undefined;
  org: string | undefined;
  gate: React.ReactNode;
} {
  const me = useHubMe();
  const search = useSearch({ strict: false }) as HubSearchParams;
  if (me.isPending)
    return { me: undefined, org: undefined, gate: <p className="text-text-muted">Loading…</p> };
  if (me.error) {
    if (me.error instanceof HubError && me.error.status === 401) {
      return {
        me: undefined,
        org: undefined,
        gate: (
          <p>
            <a className="font-medium text-accent hover:underline" href={loginUrl(page)}>
              Sign in to the Hub
            </a>{" "}
            to see your organisations.
          </p>
        ),
      };
    }
    return {
      me: undefined,
      org: undefined,
      gate: (
        <p role="alert" className="text-verdict-denied">
          The Hub is not reachable: {errorText(me.error)}
        </p>
      ),
    };
  }
  const data = me.data;
  if (!data) return { me: undefined, org: undefined, gate: null };
  const orgs = data.orgs;
  if (orgs.length === 0) {
    return {
      me: data,
      org: undefined,
      gate: <p className="text-text-muted">You are not in any organisation yet.</p>,
    };
  }
  const org = orgs.find((o) => o.slug === search.org)?.slug ?? orgs[0]?.slug;
  return { me: data, org, gate: null };
}

function OrgPicker({ me, org, page }: { me: HubMe; org: string; page: "/hub" | "/hub/search" }) {
  const navigate = useNavigate();
  const id = useId();
  if (me.orgs.length < 2) return <span className="font-mono text-sm text-text-muted">{org}</span>;
  return (
    <label htmlFor={id} className="flex items-center gap-2 text-sm">
      Organisation
      <select
        id={id}
        value={org}
        onChange={(e) => void navigate({ to: page, search: { org: e.target.value } })}
        className="rounded-sm border border-border-strong bg-surface-1 px-2 py-1"
      >
        {me.orgs.map((o) => (
          <option key={o.slug} value={o.slug}>
            {o.name}
          </option>
        ))}
      </select>
    </label>
  );
}

function Header({
  title,
  page,
  me,
  org,
}: {
  title: string;
  page: "/hub" | "/hub/search";
  me?: HubMe;
  org?: string;
}) {
  return (
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div className="flex items-baseline gap-3">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        <nav aria-label="Hub" className="flex gap-3 text-sm">
          <Link
            to="/hub"
            search={{ org }}
            className="text-accent hover:underline"
            activeOptions={{ exact: true }}
            activeProps={{ "aria-current": "page" }}
          >
            Dashboard
          </Link>
          <Link
            to="/hub/search"
            search={{ org }}
            className="text-accent hover:underline"
            activeProps={{ "aria-current": "page" }}
          >
            Search
          </Link>
        </nav>
      </div>
      {me && org && <OrgPicker me={me} org={org} page={page} />}
    </header>
  );
}

/** A headline number. */
function Stat({ label, value, note }: { label: string; value: string; note?: string }) {
  return (
    <Card className="flex flex-col gap-1">
      <span className="text-sm text-text-secondary">{label}</span>
      <span className="font-mono text-2xl font-semibold tabular-nums text-text-primary">
        {value}
      </span>
      {note && <span className="text-xs text-text-muted">{note}</span>}
    </Card>
  );
}

/** One-series horizontal bars: label, bar, value as text (the bar never carries the number alone). */
function Bars({
  title,
  rows,
  caption,
}: {
  title: string;
  rows: { label: string; value: number; text: string; hint: string }[];
  caption: string;
}) {
  const id = useId();
  const max = Math.max(0, ...rows.map((r) => r.value));
  return (
    <Card aria-labelledby={id} className="flex flex-col gap-3">
      <h2 id={id} className="text-base font-semibold">
        {title}
      </h2>
      {rows.length === 0 ? (
        <p className="text-sm text-text-muted">Nothing yet.</p>
      ) : (
        <table className="w-full text-sm">
          <caption className="sr-only">{caption}</caption>
          <tbody>
            {rows.map((r) => (
              <tr key={r.label} title={r.hint} className="hover:bg-surface-2">
                <th
                  scope="row"
                  className="w-32 py-1 pr-3 text-left font-mono font-normal text-text-secondary"
                >
                  {r.label}
                </th>
                <td className="py-1" aria-hidden="true">
                  <div
                    className="h-2.5 rounded-r-[4px] bg-accent"
                    style={{ width: `${share(r.value, max)}%` }}
                  />
                </td>
                <td className="w-24 py-1 pl-3 text-right font-mono tabular-nums text-text-primary">
                  {r.text}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}

/** Org dashboard (plan §2 step 15): sessions, divergences, denials, capture, judge. */
export function HubDashboardPage() {
  const { me, org, gate } = useOrg("/hub");
  const stats = useHubStats(org);
  const s = stats.data;
  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-5 p-4 md:p-6">
      <Header title="Hub" page="/hub" me={me} org={org} />
      {gate}
      {stats.error ? (
        <p role="alert" className="text-verdict-denied">
          {errorText(stats.error)}
        </p>
      ) : null}
      {s && (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Stat label="Sessions" value={String(s.sessions.total)} />
            <Stat label="Denied actions" value={String(s.denied.total)} />
            <Stat
              label="Event loss"
              value={String(s.capture.dropped_events)}
              note={`of ${s.capture.events} events · ${s.capture.sensor_lost} sensor losses`}
            />
            <Stat
              label="Judge cost"
              value={s.judge.recorded ? `$${s.judge.cost_usd.toFixed(2)}` : "—"}
              note={
                s.judge.recorded
                  ? `${s.judge.input_tokens + s.judge.output_tokens} tokens`
                  : "not recorded in bundles"
              }
            />
          </div>
          <div className="grid gap-3 lg:grid-cols-2">
            <Bars
              title="Divergence rate per rule"
              caption="Share of sessions that raised each rule"
              rows={s.divergences.by_rule.map((r) => ({
                label: r.rule,
                value: r.rate,
                text: percent(r.rate),
                hint: `${r.rule}: ${r.sessions} of ${s.sessions.considered} sessions, raised ${r.raised} times`,
              }))}
            />
            <Bars
              title="Sessions per repository"
              caption="Uploaded sessions per repository"
              rows={s.sessions.by_repo.map((r) => ({
                label: r.repo ?? "(none)",
                value: r.sessions,
                text: String(r.sessions),
                hint: `${r.repo ?? "no repository"}: ${r.sessions} sessions`,
              }))}
            />
            <Bars
              title="Denied actions"
              caption="Denied actions by event kind"
              rows={s.denied.by_kind.map((k) => ({
                label: k.kind.split(".").slice(-2).join("."),
                value: k.count,
                text: String(k.count),
                hint: `${k.kind}: ${k.count}`,
              }))}
            />
            <Card className="flex flex-col gap-2 text-sm">
              <h2 className="text-base font-semibold">Capture</h2>
              <p>
                Overhead:{" "}
                <span className="font-mono">
                  {s.capture.overhead_pct === null
                    ? "not recorded in bundles"
                    : `${s.capture.overhead_pct} %`}
                </span>
              </p>
              <p>
                Agents:{" "}
                {s.sessions.by_agent.map((a) => `${a.agent} (${a.sessions})`).join(", ") || "none"}
              </p>
            </Card>
          </div>
        </>
      )}
    </div>
  );
}

/** Search sessions and events: `path:~/.ssh rule:D003 repo:shop`. */
export function HubSearchPage() {
  const { me, org, gate } = useOrg("/hub/search");
  const search = useSearch({ strict: false }) as HubSearchParams;
  const navigate = useNavigate();
  const [draft, setDraft] = useState(search.q ?? "");
  const q = search.q ?? "";
  const result = useHubSearch(org, q);
  const inputId = useId();
  const errorId = useId();
  const err = result.error instanceof HubError ? result.error : null;
  function submit(e: FormEvent) {
    e.preventDefault();
    void navigate({ to: "/hub/search", search: { org, q: draft } });
  }
  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-5 p-4 md:p-6">
      <Header title="Hub search" page="/hub/search" me={me} org={org} />
      {gate}
      {org && (
        <form role="search" onSubmit={submit} className="flex flex-col gap-2">
          <label htmlFor={inputId} className="text-sm text-text-secondary">
            Filters:{" "}
            <code className="font-mono">
              path: rule: repo: agent: session: kind: host: exe: tool:
            </code>{" "}
            and words
          </label>
          <div className="flex gap-2">
            <input
              id={inputId}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder="path:~/.ssh rule:D003 repo:shop"
              aria-invalid={err?.status === 400}
              aria-describedby={err ? errorId : undefined}
              className="min-w-0 flex-1 rounded-sm border border-border-strong bg-surface-1 px-3 py-1.5 font-mono text-sm"
            />
            <button
              type="submit"
              className="rounded-sm bg-accent px-3 py-1.5 text-sm font-medium text-on-accent hover:bg-accent-hover"
            >
              Search
            </button>
          </div>
          {err && (
            <p id={errorId} role="alert" className="text-sm text-verdict-denied">
              {err.status === 400 && err.position !== undefined ? (
                <>
                  {err.detail} — <code className="font-mono">{q.slice(0, err.position)}</code>
                  <mark className="bg-accent-subtle font-mono">{q.slice(err.position) || " "}</mark>
                </>
              ) : (
                err.message
              )}
            </p>
          )}
        </form>
      )}
      {result.data && (
        <>
          <section aria-label="Matching sessions" className="flex flex-col gap-2">
            <h2 className="text-base font-semibold">Sessions ({result.data.sessions.length})</h2>
            <ul className="flex flex-col gap-1 text-sm">
              {result.data.sessions.map((s) => (
                <li key={s.id} className="flex flex-wrap gap-x-3">
                  <span className="font-medium">{s.session.name ?? s.session.id}</span>
                  <span className="font-mono text-text-muted">{s.repo ?? "—"}</span>
                  <span className="font-mono text-text-secondary">{s.rules.join(" ")}</span>
                </li>
              ))}
            </ul>
          </section>
          {result.data.events_searched && (
            <section aria-label="Matching events" className="flex flex-col gap-2">
              <h2 className="text-base font-semibold">Events ({result.data.hits.length})</h2>
              <table className="w-full text-left text-sm">
                <thead className="text-text-secondary">
                  <tr>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      Session
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      Seq
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      Kind
                    </th>
                    <th scope="col" className="py-1 font-medium">
                      What
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {result.data.hits.map((h) => (
                    <tr key={`${h.bundle}-${h.seq}`} className="border-t border-border">
                      <td className="py-1 pr-3">{h.session}</td>
                      <td className="py-1 pr-3 font-mono tabular-nums">{h.seq}</td>
                      <td className="py-1 pr-3 font-mono">{h.kind}</td>
                      <td className="py-1 font-mono break-all">
                        {[h.path, h.target, h.detail].filter(Boolean).join(" · ")}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </section>
          )}
        </>
      )}
    </div>
  );
}
