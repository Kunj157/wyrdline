/**
 * Pure views of a session for Studio: how to label each event, where it
 * sits in the river, which effects cannot be undone, and the transcript.
 * No React, no I/O, so all of it is unit-tested.
 */
import type { Event } from "@wyrdline/schemas";
import type { api } from "@wyrdline/schemas";
import type { Meaning } from "@wyrdline/ui";

export type Plane = "meta" | "intent" | "effect" | "state" | "guard" | "verdict";

export function planeOf(e: Event): Plane {
  return e.body.kind.split(".")[0] as Plane;
}

/** A short human description of one event. */
export function describe(e: Event): string {
  const b = e.body;
  switch (b.kind) {
    case "intent.tool_call.started":
      return `${b.payload.tool} ${argsPreview(b.payload.args)}`.trim();
    case "intent.tool_call.finished":
      return `${b.payload.status}${b.payload.exit_code == null ? "" : ` (exit ${b.payload.exit_code})`}`;
    case "intent.tool_call.denied":
      return `denied: ${b.payload.reason}`;
    case "intent.message":
      return `${b.payload.role}: ${b.payload.text.slice(0, 120)}`;
    case "effect.proc.exec":
      return `exec ${b.payload.argv.join(" ") || b.payload.filename}`;
    case "effect.proc.exit":
      return `exit ${b.payload.exit_code}`;
    case "effect.proc.fork":
      return `fork → ${b.payload.child_pid}`;
    case "effect.file.open_write":
    case "effect.file.write":
    case "effect.file.unlink":
    case "effect.file.chmod":
    case "effect.file.truncate":
      return `${b.kind.split(".")[2]} ${b.payload.file.path}`;
    case "effect.file.rename":
      return `rename ${b.payload.from.path} → ${b.payload.to.path}`;
    case "effect.file.mkdir":
      return `mkdir ${b.payload.path}`;
    case "effect.file.read_sensitive":
      return `read secret ${b.payload.path}`;
    case "effect.net.connect":
      return `connect ${b.payload.dns_name ?? b.payload.addr}:${b.payload.port} (${b.payload.verdict})`;
    case "effect.net.dns_query":
      return `dns ${b.payload.name}`;
    case "state.checkpoint.created":
      return `checkpoint ${b.payload.checkpoint_id} (${b.payload.files_changed} ${b.payload.files_changed === 1 ? "file" : "files"})`;
    case "state.checkpoint.rewound":
      return `rewound to ${b.payload.checkpoint_id} → branch ${b.payload.branch_id}`;
    case "state.checkpoint.forked":
      return `forked ${b.payload.name} from ${b.payload.checkpoint_id}`;
    case "guard.decision":
      return `${b.payload.decision}: ${b.payload.reason}`;
    case "meta.session.state_changed":
      return `${b.payload.from} → ${b.payload.to}`;
    case "meta.rewind.requested":
      return `agent asks to rewind to ${b.payload.checkpoint_id}`;
    case "meta.rewind.confirmed":
      return `rewind to ${b.payload.checkpoint_id} confirmed (${b.payload.by})`;
    case "meta.rewind.rejected":
      return `rewind request ${b.payload.request_seq} rejected (${b.payload.by})`;
    default:
      return b.kind;
  }
}

function argsPreview(args: unknown): string {
  if (args && typeof args === "object") {
    const o = args as Record<string, unknown>;
    for (const k of ["command", "file_path", "path", "pattern", "url"]) {
      if (typeof o[k] === "string") return String(o[k]).slice(0, 80);
    }
  }
  return "";
}

/** The meaning colour of an event, for badges and knots. */
export function meaningOf(e: Event): Meaning {
  const b = e.body;
  switch (b.kind) {
    case "intent.tool_call.denied":
      return "denied";
    case "effect.net.connect":
      return b.payload.verdict === "denied" ? "denied" : "irreversible";
    case "guard.decision":
      return b.payload.decision === "allow" ? "ok" : "denied";
    case "verdict.divergence.raised":
      return "divergence";
    default:
      break;
  }
  if (isIrreversible(e)) return "irreversible";
  const plane = planeOf(e);
  return plane === "intent" ? "intent" : plane === "effect" ? "effect" : "unknown";
}

/**
 * Effects a rewind cannot undo: network egress and writes outside the
 * Cell (plan §17.2 "escaped effects").
 */
export function isIrreversible(e: Event): boolean {
  const b = e.body;
  switch (b.kind) {
    case "effect.net.connect":
      return b.payload.verdict === "allowed";
    case "effect.file.open_write":
    case "effect.file.write":
    case "effect.file.unlink":
    case "effect.file.chmod":
    case "effect.file.truncate":
      return !b.payload.file.in_cell;
    case "effect.file.rename":
      return !b.payload.from.in_cell || !b.payload.to.in_cell;
    default:
      return false;
  }
}

/** Irreversible effects recorded after `seq` (what a rewind to it leaves behind). */
export function irreversibleAfter(events: Event[], seq: number): Event[] {
  return events.filter((e) => e.seq > seq && isIrreversible(e));
}

export interface Turn {
  seq: number;
  role: string;
  turn: number;
  text: string;
}

export function transcript(events: Event[]): Turn[] {
  const out: Turn[] = [];
  for (const e of events) {
    if (e.body.kind === "intent.message") {
      const { role, turn, text } = e.body.payload;
      out.push({ seq: e.seq, role, turn, text });
    }
  }
  return out;
}

/** One knot of the river. */
export interface Knot {
  seq: number;
  lane: "intent" | "spine" | "effect";
  x: number;
  meaning: Meaning;
  label: string;
  callId: string | null;
}

/** A thread from a tool call to one effect it caused. */
export interface Thread {
  from: number;
  to: number;
  callId: string;
  ambiguous: boolean;
}

export interface RiverLayout {
  knots: Knot[];
  threads: Thread[];
  width: number;
}

/**
 * Lay the session out left to right in event order: tool calls on the
 * intent lane, checkpoints on the spine, effects below, and a thread from
 * each call to every effect attributed to it.
 */
export function layoutRiver(events: Event[], graph: api.Graph | undefined, step = 14): RiverLayout {
  const callOf = new Map<number, string>();
  for (const c of graph?.calls ?? []) callOf.set(c.started_seq, c.call_id);
  const effectOf = new Map<number, api.Effect>();
  for (const f of graph?.effects ?? []) effectOf.set(f.seq, f);
  const knots: Knot[] = [];
  const startX = new Map<string, number>();
  let x = step;
  for (const e of events) {
    const k = e.body.kind;
    let lane: Knot["lane"] | null = null;
    let callId: string | null = null;
    if (k === "intent.tool_call.started" || k === "intent.tool_call.denied") {
      lane = "intent";
      callId =
        callOf.get(e.seq) ?? (k === "intent.tool_call.denied" ? e.body.payload.call_id : null);
    } else if (
      k === "state.checkpoint.created" ||
      k === "state.checkpoint.rewound" ||
      k === "state.checkpoint.forked"
    ) {
      lane = "spine";
    } else if (planeOf(e) === "effect") {
      lane = "effect";
      callId = effectOf.get(e.seq)?.call_id ?? null;
    }
    if (!lane) continue;
    knots.push({ seq: e.seq, lane, x, meaning: meaningOf(e), label: describe(e), callId });
    if (lane === "intent" && callId) startX.set(callId, x);
    x += step;
  }
  const seqOfCall = new Map<string, number>();
  for (const kn of knots) if (kn.lane === "intent" && kn.callId) seqOfCall.set(kn.callId, kn.seq);
  const threads: Thread[] = [];
  for (const f of graph?.effects ?? []) {
    const ids = f.call_id ? [f.call_id] : f.candidates;
    for (const id of ids) {
      const from = seqOfCall.get(id);
      if (from !== undefined) threads.push({ from, to: f.seq, callId: id, ambiguous: !f.call_id });
    }
  }
  return { knots, threads, width: x };
}
