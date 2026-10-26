/** Daemon data for Studio: one client and TanStack Query hooks around it. */
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Event } from "@wyrdline/schemas";
import type * as api from "@wyrdline/schemas/generated-api";
import { type Client, createClient } from "@wyrdline/schemas/client";
import { useSyncExternalStore } from "react";

import { takeTokenFromUrl } from "./auth";
import type { BundleInfo } from "./bundle";

let client: Client | null = null;
let token: string | null = null;

export function apiClient(): Client {
  if (!client) {
    token = typeof window === "undefined" ? null : takeTokenFromUrl();
    client = createClient(token ? { token } : {});
  }
  return client;
}

/** The bearer for requests made outside the client (the live stream). */
export function apiToken(): string | null {
  apiClient();
  return token;
}

/** Tests and embedding: use a specific client. */
export function setApiClient(c: Client): void {
  client = c;
}

let offlineName: string | null = null;
let bundleInfo: BundleInfo | null = null;

/**
 * Switch Studio to a read-only replay of an exported session, or of a
 * verified bundle (`bundle`).
 */
export function setOffline(name: string, c: Client, bundle: BundleInfo | null = null): void {
  offlineName = name;
  bundleInfo = bundle;
  client = c;
  offlineState = { name, bundle };
  for (const l of listeners) l();
}

let offlineState: { name: string; bundle: BundleInfo | null } | null = null;
const listeners = new Set<() => void>();

/** The offline file and bundle, re-rendering when Studio switches to one. */
export function useOffline(): { name: string; bundle: BundleInfo | null } | null {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => offlineState,
  );
}

/** The open bundle in bundle mode (verified), else `null`. */
export function offlineBundle(): BundleInfo | null {
  return bundleInfo;
}

/** The replayed file's name in offline mode, else `null`. */
export function offlineSource(): string | null {
  return offlineName;
}

export const keys = {
  sessions: ["sessions"] as const,
  session: (sid: string) => ["session", sid] as const,
  events: (sid: string) => ["events", sid] as const,
  graph: (sid: string) => ["graph", sid] as const,
  verify: (sid: string) => ["verify", sid] as const,
  checkpoints: (sid: string) => ["checkpoints", sid] as const,
  asks: (sid: string) => ["asks", sid] as const,
  divergences: (sid: string) => ["divergences", sid] as const,
  policies: ["policies"] as const,
  diff: (sid: string, a: string, b: string) => ["diff", sid, a, b] as const,
  diffFile: (sid: string, a: string, b: string, path: string) =>
    ["diff-file", sid, a, b, path] as const,
};

export function useSessions() {
  return useQuery({ queryKey: keys.sessions, queryFn: () => apiClient().sessions() });
}

export function useCapabilities() {
  return useQuery({
    queryKey: ["capabilities"],
    queryFn: () => apiClient().capabilities(),
    staleTime: 60_000,
  });
}

export function useSession(sid: string) {
  return useQuery({ queryKey: keys.session(sid), queryFn: () => apiClient().session(sid) });
}

/** Every event of a session, page by page. */
export async function fetchAllEvents(c: Client, sid: string): Promise<Event[]> {
  const all: Event[] = [];
  let after: number | undefined;
  for (;;) {
    const page = await c.events(sid, { after_seq: after, limit: 5000 });
    all.push(...page.events);
    if (page.next_after_seq == null) return all;
    after = page.next_after_seq;
  }
}

export function useEvents(sid: string) {
  return useQuery({ queryKey: keys.events(sid), queryFn: () => fetchAllEvents(apiClient(), sid) });
}

export function useGraph(sid: string) {
  return useQuery({ queryKey: keys.graph(sid), queryFn: () => apiClient().graph(sid) });
}

export function useVerify(sid: string) {
  return useQuery({ queryKey: keys.verify(sid), queryFn: () => apiClient().verify(sid) });
}

export function useCheckpoints(sid: string) {
  return useQuery({ queryKey: keys.checkpoints(sid), queryFn: () => apiClient().checkpoints(sid) });
}

export function useDiff(sid: string, a: string | null, b: string | null) {
  return useQuery({
    queryKey: keys.diff(sid, a ?? "", b ?? ""),
    queryFn: () => apiClient().diff(sid, a ?? "", b ?? ""),
    enabled: Boolean(a && b),
  });
}

export function useDiffFile(sid: string, a: string, b: string, path: string | null) {
  return useQuery({
    queryKey: keys.diffFile(sid, a, b, path ?? ""),
    queryFn: () => apiClient().diffFile(sid, a, b, path ?? ""),
    enabled: Boolean(path),
  });
}

function newKey(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random()}`;
}

/** Rewind, fork, apply, and checkpoint, each retry-safe (idempotency key). */
export function useSessionActions(sid: string) {
  const qc = useQueryClient();
  const refresh = () =>
    Promise.all([
      qc.invalidateQueries({ queryKey: keys.session(sid) }),
      qc.invalidateQueries({ queryKey: keys.checkpoints(sid) }),
      qc.invalidateQueries({ queryKey: keys.events(sid) }),
    ]);
  const c = apiClient();
  return {
    rewind: useMutation({
      mutationFn: (v: { target: string; relaunch: boolean }) =>
        c.rewind(sid, v.target, { relaunch: v.relaunch }, newKey()),
      onSuccess: refresh,
    }),
    fork: useMutation({
      mutationFn: (v: { target: string; name: string }) =>
        c.fork(sid, v.target, { name: v.name }, newKey()),
      onSuccess: refresh,
    }),
    apply: useMutation({
      mutationFn: (v: { target: string; dry_run: boolean; force: boolean }) =>
        c.apply(sid, v.target, { dry_run: v.dry_run, force: v.force }, newKey()),
      onSuccess: refresh,
    }),
    checkpoint: useMutation({ mutationFn: () => c.checkpoint(sid, newKey()), onSuccess: refresh }),
  };
}

/** Questions the running session's policy is waiting on (polled while live). */
export function useAsks(sid: string, live: boolean) {
  return useQuery({
    queryKey: keys.asks(sid),
    queryFn: () => apiClient().asks(sid),
    enabled: live,
    refetchInterval: live ? 1000 : false,
  });
}

export function useAnswerAsk(sid: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (v: { id: number; allow: boolean }) => apiClient().answerAsk(sid, v.id, v.allow),
    onSettled: () => qc.invalidateQueries({ queryKey: keys.asks(sid) }),
  });
}

/** A session's divergences with their triage state. */
export function useDivergences(sid: string) {
  return useQuery({
    queryKey: keys.divergences(sid),
    queryFn: () => apiClient().divergences(sid),
  });
}

/** Every session's divergences, one request per session (the inbox). */
export function useAllDivergences(sessions: api.SessionRecord[]) {
  return useQueries({
    queries: sessions.map((s) => ({
      queryKey: keys.divergences(s.id),
      queryFn: () => apiClient().divergences(s.id),
    })),
  });
}

/**
 * Acknowledge, mark a false positive, or reopen. Only a success refreshes
 * the divergences (and the events, which gain the update); a refusal
 * leaves everything as it was.
 */
export function useTriage(sid: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (v: { did: string; body: api.TriageBody }) =>
      apiClient().triage(sid, v.did, v.body),
    onSuccess: () =>
      Promise.all([
        qc.invalidateQueries({ queryKey: keys.divergences(sid) }),
        qc.invalidateQueries({ queryKey: keys.events(sid) }),
      ]),
  });
}

export function usePolicies(enabled: boolean) {
  return useQuery({
    queryKey: keys.policies,
    queryFn: () => apiClient().policies(),
    enabled,
  });
}

/** Replay this session against an uploaded policy. */
export function useBacktest(sid: string) {
  return useMutation({
    mutationFn: (policyId: string) => apiClient().backtest(policyId, sid),
  });
}
