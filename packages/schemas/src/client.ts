/**
 * Typed client for the wyrdd daemon API (docs/api/openapi.json). Request
 * and response types are generated from the Rust handlers; this file only
 * builds URLs and headers.
 */
import type * as api from "./generated-api/index";

export interface ClientOptions {
  /** e.g. `http://127.0.0.1:7717`; empty for same-origin (Studio served by wyrdd). */
  baseUrl?: string | undefined;
  /** Daemon bearer token (TCP). Omit on the same origin over the unix socket. */
  token?: string | undefined;
  fetch?: typeof fetch;
}

/** A non-2xx answer, carrying the RFC 9457 problem body. */
export class ApiError extends Error {
  readonly status: number;
  readonly problem: api.Problem;

  constructor(status: number, problem: api.Problem) {
    super(`${status} ${problem.title}${problem.detail ? `: ${problem.detail}` : ""}`);
    this.name = "ApiError";
    this.status = status;
    this.problem = problem;
  }
}

export interface SessionQuery {
  status?: string | undefined;
  agent?: string | undefined;
  repo?: string | undefined;
  since?: number | undefined;
}

export interface EventQuery {
  plane?: string | undefined;
  kind?: string | undefined;
  after_seq?: number | undefined;
  limit?: number | undefined;
}

export interface RangeQuery {
  from_seq?: number | undefined;
  to_seq?: number | undefined;
}

type Query = Record<string, string | number | undefined>;

function query(q: Query | undefined): string {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q ?? {})) {
    if (v !== undefined) params.set(k, String(v));
  }
  const s = params.toString();
  return s ? `?${s}` : "";
}

const seg = encodeURIComponent;

export function createClient(options: ClientOptions = {}) {
  const base = options.baseUrl ?? "";
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);

  async function request<T>(
    method: "GET" | "POST",
    path: string,
    body?: unknown,
    idempotencyKey?: string,
  ): Promise<T> {
    const headers: Record<string, string> = { accept: "application/json" };
    if (options.token) headers.authorization = `Bearer ${options.token}`;
    if (body !== undefined) headers["content-type"] = "application/json";
    if (idempotencyKey) headers["idempotency-key"] = idempotencyKey;
    const init: RequestInit = { method, headers };
    if (body !== undefined) init.body = JSON.stringify(body);
    const res = await doFetch(`${base}${path}`, init);
    const text = await res.text();
    const json: unknown = text ? JSON.parse(text) : null;
    if (!res.ok) {
      const problem = (json ?? {
        type: "unknown",
        title: res.statusText,
        status: res.status,
        detail: null,
      }) as api.Problem;
      throw new ApiError(res.status, problem);
    }
    return json as T;
  }

  const s = (sid: string) => `/v1/sessions/${seg(sid)}`;

  return {
    health: () => request<api.Health>("GET", "/v1/health"),
    ready: () => request<api.Ready>("GET", "/v1/ready"),
    capabilities: () => request<api.Report>("GET", "/v1/capabilities"),
    sessions: (q?: SessionQuery) =>
      request<api.SessionList>("GET", `/v1/sessions${query(q as Query)}`),
    session: (sid: string) => request<api.SessionDetail>("GET", s(sid)),
    events: (sid: string, q?: EventQuery) =>
      request<api.EventPage>("GET", `${s(sid)}/events${query(q as Query)}`),
    graph: (sid: string, q?: RangeQuery) =>
      request<api.Graph>("GET", `${s(sid)}/graph${query(q as Query)}`),
    toolCall: (sid: string, callId: string) =>
      request<api.ToolCallDetail>("GET", `${s(sid)}/tool-calls/${seg(callId)}`),
    verify: (sid: string) => request<api.VerifyReport>("GET", `${s(sid)}/verify`),
    checkpoints: (sid: string) => request<api.Checkpoints>("GET", `${s(sid)}/checkpoints`),
    checkpoint: (sid: string, idempotencyKey?: string) =>
      request<api.CheckpointRecord>("POST", `${s(sid)}/checkpoints`, {}, idempotencyKey),
    stop: (sid: string, idempotencyKey?: string) =>
      request<api.Stopped>("POST", `${s(sid)}/stop`, {}, idempotencyKey),
    asks: (sid: string) => request<api.PendingAsks>("GET", `${s(sid)}/asks`),
    answerAsk: (sid: string, id: number, allow: boolean) =>
      request<api.Answered>("POST", `${s(sid)}/asks/${id}`, { allow } satisfies api.AnswerBody),
    rewind: (sid: string, target: string, body: api.RewindBody, idempotencyKey?: string) =>
      request<api.BranchCreated | api.Relaunching>(
        "POST",
        `${s(sid)}/checkpoints/${seg(target)}/rewind`,
        body,
        idempotencyKey,
      ),
    fork: (sid: string, target: string, body: api.ForkBody, idempotencyKey?: string) =>
      request<api.BranchCreated | api.Relaunching>(
        "POST",
        `${s(sid)}/checkpoints/${seg(target)}/fork`,
        body,
        idempotencyKey,
      ),
    apply: (sid: string, target: string, body: api.ApplyBody, idempotencyKey?: string) =>
      request<api.ApplyReport>(
        "POST",
        `${s(sid)}/checkpoints/${seg(target)}/apply`,
        body,
        idempotencyKey,
      ),
    diff: (sid: string, a: string, b: string) =>
      request<api.Changes>("GET", `${s(sid)}/diff/${seg(a)}/${seg(b)}`),
    diffFile: (sid: string, a: string, b: string, path: string) =>
      request<api.FileDiff>("GET", `${s(sid)}/diff/${seg(a)}/${seg(b)}/file${query({ path })}`),
    /** URL for an `EventSource`; resume with `afterSeq` (or the browser's Last-Event-ID). */
    streamUrl: (sid: string, afterSeq?: number) =>
      `${base}${s(sid)}/stream${query({ after_seq: afterSeq })}`,
  };
}

export type Client = ReturnType<typeof createClient>;
