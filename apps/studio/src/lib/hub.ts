/**
 * Hub mode: Studio served by (or next to) a Wyrdline Hub talks to its
 * `/api/v1` with the Hub's session cookie. Types mirror the Hub's JSON
 * (docs/architecture/hub.md).
 */
import { useQuery } from "@tanstack/react-query";

export type HubOrg = { slug: string; name: string; role: "viewer" | "member" | "admin" };
export type HubMe = { id: string; email: string | null; name: string | null; orgs: HubOrg[] };

export type HubStats = {
  sessions: {
    total: number;
    considered: number;
    by_repo: { repo: string | null; sessions: number }[];
    by_agent: { agent: string; sessions: number }[];
  };
  divergences: { by_rule: { rule: string; sessions: number; raised: number; rate: number }[] };
  denied: { total: number; by_kind: { kind: string; count: number }[] };
  capture: {
    events: number;
    dropped_events: number;
    sensor_lost: number;
    overhead_pct: number | null;
  };
  judge: {
    recorded: boolean;
    runs: number;
    input_tokens: number;
    output_tokens: number;
    cost_usd: number;
  };
};

export type HubSession = {
  id: string;
  repo: string | null;
  rules: string[];
  session: { id: string; name: string | null; agent: string };
  created_ms: number;
};

export type HubHit = {
  bundle: string;
  session: string | null;
  repo: string | null;
  seq: number;
  kind: string;
  path: string | null;
  target: string | null;
  detail: string | null;
};

export type HubSearch = { sessions: HubSession[]; hits: HubHit[]; events_searched: boolean };

/** An error answer of the Hub (RFC 9457 problem details). */
export class HubError extends Error {
  constructor(
    public status: number,
    public title: string,
    public detail?: string,
    public position?: number,
  ) {
    super(detail ? `${title}: ${detail}` : title);
  }
}

/** Base URL of the Hub API: same origin unless configured at build time. */
export function hubBase(): string {
  return (import.meta.env.VITE_WYRD_HUB_URL as string | undefined)?.replace(/\/$/, "") ?? "";
}

export async function hubGet<T>(path: string): Promise<T> {
  const res = await fetch(`${hubBase()}/api/v1${path}`, {
    credentials: "include",
    headers: { accept: "application/json" },
  });
  const body: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const p = (body ?? {}) as { title?: string; detail?: string; position?: number };
    throw new HubError(res.status, p.title ?? `HTTP ${res.status}`, p.detail, p.position);
  }
  return body as T;
}

export function loginUrl(returnTo: string): string {
  return `${hubBase()}/api/v1/auth/oidc/login?return_to=${encodeURIComponent(returnTo)}`;
}

const noRetryOn4xx = (n: number, e: unknown) => n < 2 && !(e instanceof HubError && e.status < 500);

export function useHubMe() {
  return useQuery({
    queryKey: ["hub", "me"],
    queryFn: () => hubGet<HubMe>("/me"),
    retry: noRetryOn4xx,
  });
}

export function useHubStats(org: string | undefined) {
  return useQuery({
    queryKey: ["hub", "stats", org],
    queryFn: () => hubGet<HubStats>(`/orgs/${encodeURIComponent(org ?? "")}/stats`),
    enabled: !!org,
    retry: noRetryOn4xx,
  });
}

export function useHubSearch(org: string | undefined, q: string) {
  return useQuery({
    queryKey: ["hub", "search", org, q],
    queryFn: () =>
      hubGet<HubSearch>(`/orgs/${encodeURIComponent(org ?? "")}/search?q=${encodeURIComponent(q)}`),
    enabled: !!org && q.trim() !== "",
    retry: noRetryOn4xx,
  });
}

/** `0.6667` → `67 %`. */
export function percent(rate: number): string {
  return `${Math.round(rate * 100)} %`;
}

/** Bar width for `value` of `max`, never zero-width for a non-zero value. */
export function share(value: number, max: number): number {
  if (max <= 0 || value <= 0) return 0;
  return Math.max(2, Math.round((value / max) * 100));
}
