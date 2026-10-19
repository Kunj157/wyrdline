/** Daemon data for Studio: one client and TanStack Query hooks around it. */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Event } from "@wyrdline/schemas";
import { type Client, createClient } from "@wyrdline/schemas/client";

import { takeTokenFromUrl } from "./auth";

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

/** Switch Studio to a read-only replay of an exported session. */
export function setOffline(name: string, c: Client): void {
  offlineName = name;
  client = c;
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
  rewindRequests: (sid: string) => ["rewind-requests", sid] as const,
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

/** Rewind requests the agent made through `wyrd mcp` (polled while live). */
export function useRewindRequests(sid: string, live: boolean) {
  return useQuery({
    queryKey: keys.rewindRequests(sid),
    queryFn: () => apiClient().rewindRequests(sid),
    refetchInterval: live ? 2000 : false,
  });
}

/** Confirm (rewind) or reject one of the agent's rewind requests. */
export function useDecideRewind(sid: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (v: { id: number; confirm: boolean }) =>
      apiClient().decideRewind(sid, v.id, { confirm: v.confirm, note: null }),
    onSettled: () =>
      Promise.all([
        qc.invalidateQueries({ queryKey: keys.rewindRequests(sid) }),
        qc.invalidateQueries({ queryKey: keys.session(sid) }),
        qc.invalidateQueries({ queryKey: keys.checkpoints(sid) }),
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
