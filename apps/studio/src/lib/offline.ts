/**
 * Offline mode: replay an exported session (`wyrd events <session> --json`,
 * one event per line, or a JSON array) without a daemon. The causal graph is
 * computed in the browser by the same Rust correlator the daemon uses,
 * compiled to WebAssembly (`wyrd-wasm`). Everything is read-only: actions,
 * live streaming, chain verification, file diffs, and divergence triage need
 * the daemon (and the session's content store), so they are disabled and say
 * so. Divergences are folded from the exported verdict events.
 */
import type { Event } from "@wyrdline/schemas";
import { ApiError, type Client } from "@wyrdline/schemas/client";
import type * as api from "@wyrdline/schemas/generated-api";

import { projectDivergences } from "./divergences";

/** Parse an export: a JSON array of events or JSON lines. */
export function parseExport(text: string): Event[] {
  const trimmed = text.trim();
  if (!trimmed) throw new Error("the file is empty");
  const events: Event[] = trimmed.startsWith("[")
    ? (JSON.parse(trimmed) as Event[])
    : trimmed
        .split("\n")
        .filter((l) => l.trim())
        .map((l, i) => {
          try {
            return JSON.parse(l) as Event;
          } catch {
            throw new Error(`line ${i + 1} is not a JSON event`);
          }
        });
  if (
    !events.every((e) => typeof e.seq === "number" && e.body && typeof e.body.kind === "string")
  ) {
    throw new Error("not a Wyrdline event export (expected `wyrd events <session> --json`)");
  }
  return [...events].sort((a, b) => a.seq - b.seq);
}

/** Session details reconstructed from the events alone. */
export function sessionFromEvents(events: Event[], fallbackName: string): api.SessionDetail {
  let id = fallbackName;
  let workspace = "";
  let command: string[] = [];
  let agent = "unknown";
  let mode: api.CaptureMode = "cell";
  let status: api.SessionState = "closed";
  let createdMs = events[0]?.ts_wall_ms ?? 0;
  let current = "a";
  const branches: api.Branch[] = [{ id: "a", name: "a", forked_from: null, created_ms: createdMs }];
  const checkpoints: api.CheckpointRecord[] = [];
  for (const e of events) {
    const b = e.body;
    switch (b.kind) {
      case "meta.session.started":
        id = b.payload.session_id;
        workspace = b.payload.workspace_root;
        command = b.payload.command;
        agent = b.payload.agent_kind;
        mode = b.payload.mode;
        createdMs = e.ts_wall_ms;
        break;
      case "meta.session.state_changed":
        status = b.payload.to;
        break;
      case "state.checkpoint.created":
        checkpoints.push({
          id: b.payload.checkpoint_id,
          branch: b.payload.branch_id,
          parent: b.payload.parent_id,
          manifest: b.payload.manifest,
          seq: e.seq,
          created_ms: e.ts_wall_ms,
          files: b.payload.files_changed,
          bytes_read: b.payload.bytes_added,
        });
        current = b.payload.branch_id;
        break;
      case "state.checkpoint.rewound":
      case "state.checkpoint.forked": {
        const name = b.kind === "state.checkpoint.forked" ? b.payload.name : b.payload.branch_id;
        if (!branches.some((x) => x.id === b.payload.branch_id)) {
          branches.push({
            id: b.payload.branch_id,
            name,
            forked_from: b.payload.checkpoint_id,
            created_ms: e.ts_wall_ms,
          });
        }
        current = b.payload.branch_id;
        break;
      }
      default:
        break;
    }
  }
  // A replay never runs: show running sessions as they were last seen, but
  // never as live.
  if (status === "recording" || status === "degraded" || status === "preparing") status = "paused";
  const last = events.at(-1);
  return {
    head_seq: last?.seq ?? 0,
    head_hash: "",
    sealed: events.some((e) => e.body.kind === "meta.session.closed"),
    id,
    name: fallbackName,
    workspace,
    command,
    mode,
    created_ms: createdMs,
    status,
    current_branch: current,
    branches,
    checkpoints,
    cgroup: null,
    pid: null,
    runs: 1,
    agent,
    deny_tools: [],
    policy: null,
  };
}

function unavailable(what: string): never {
  throw new ApiError(501, {
    type: "about:blank",
    title: "Not available offline",
    status: 501,
    detail: `${what} needs the daemon; this is a read-only replay.`,
  } as api.Problem);
}

/** A client over one exported session; `correlate` is the Wasm correlator. */
export function createOfflineClient(
  events: Event[],
  name: string,
  correlate: ((eventsJson: string) => string) | null,
): Client {
  const detail = sessionFromEvents(events, name);
  const sid = detail.id;
  const check = (s: string) => {
    if (s !== sid) {
      throw new ApiError(404, {
        type: "about:blank",
        title: "Not found",
        status: 404,
        detail: `session ${s} is not in this file`,
      } as api.Problem);
    }
  };
  let graph: api.Graph | null = null;
  const summary: api.SessionRecord = {
    id: detail.id,
    name: detail.name,
    workspace: detail.workspace,
    command: detail.command,
    mode: detail.mode,
    created_ms: detail.created_ms,
    status: detail.status,
    current_branch: detail.current_branch,
    branches: detail.branches,
    checkpoints: detail.checkpoints,
    cgroup: null,
    pid: null,
    runs: detail.runs,
    agent: detail.agent,
    deny_tools: [],
    policy: null,
  } as api.SessionRecord;
  const client = {
    health: async () => ({ status: "offline" }) as unknown as api.Health,
    ready: async () => unavailable("Readiness"),
    capabilities: async () => unavailable("Kernel capabilities"),
    sessions: async () => ({ sessions: [summary] }) as api.SessionList,
    session: async (s: string) => (check(s), detail),
    events: async (
      s: string,
      q?: { after_seq?: number | undefined; limit?: number | undefined },
    ) => {
      check(s);
      const after = q?.after_seq ?? 0;
      const limit = q?.limit ?? 5000;
      const page = events.filter((e) => e.seq > after).slice(0, limit);
      const more = events.some((e) => e.seq > (page.at(-1)?.seq ?? after)) && page.length === limit;
      return {
        events: page,
        next_after_seq: more ? (page.at(-1)?.seq ?? null) : null,
      } as api.EventPage;
    },
    graph: async (s: string) => {
      check(s);
      if (!correlate) unavailable("The causal graph (WebAssembly correlator not in this build)");
      graph ??= JSON.parse(correlate(JSON.stringify(events))) as api.Graph;
      return graph;
    },
    toolCall: async () => unavailable("Tool-call details"),
    verify: async () => unavailable("Chain verification (exports carry no hashes)"),
    checkpoints: async (s: string) => {
      check(s);
      return {
        current_branch: detail.current_branch,
        branches: detail.branches,
        checkpoints: detail.checkpoints,
      } as api.Checkpoints;
    },
    checkpoint: async () => unavailable("Checkpointing"),
    stop: async () => unavailable("Stopping"),
    rewind: async () => unavailable("Rewind"),
    fork: async () => unavailable("Fork"),
    apply: async () => unavailable("Apply"),
    diff: async () => unavailable("Diffs (they need the session's content store)"),
    diffFile: async () => unavailable("File diffs"),
    divergences: async (s: string) => {
      check(s);
      return { divergences: projectDivergences(events) } as api.DivergenceList;
    },
    triage: async () => unavailable("Triage"),
    streamUrl: () => "",
  };
  return client as unknown as Client;
}

/** Load the WebAssembly correlator if this build bundles it. */
export async function loadCorrelator(): Promise<((eventsJson: string) => string) | null> {
  // The artefact is generated by `make studio-wasm`; builds without it still
  // work and report the graph as unavailable.
  const mods = import.meta.glob("../wasm/wyrd_wasm.js");
  const load = Object.values(mods)[0];
  if (!load) return null;
  const mod = (await load()) as {
    default: () => Promise<unknown>;
    correlate: (j: string) => string;
  };
  await mod.default();
  return mod.correlate;
}
