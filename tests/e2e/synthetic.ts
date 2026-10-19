/**
 * A synthetic session of `n` events for performance runs, served by
 * intercepting the daemon API: every tenth event starts a tool call and
 * the rest are file writes attributed to it.
 */
import type { Page, Route } from "@playwright/test";

interface Ev {
  seq: number;
  ts_mono_ns: number;
  ts_wall_ms: number;
  process: { pid: number; tid: number; ppid: number; start_ns: number; cgroup_id: number } | null;
  drops_since_last: number;
  body: { kind: string; payload: unknown };
}

export function syntheticSession(n: number) {
  const events: Ev[] = [];
  const calls: unknown[] = [];
  const effects: unknown[] = [];
  let call = "";
  let callEffects: number[] = [];
  const proc = { pid: 4000, tid: 4000, ppid: 1, start_ns: 1, cgroup_id: 1 };
  for (let seq = 1; seq <= n; seq++) {
    const base = {
      seq,
      ts_mono_ns: seq * 1_000_000,
      ts_wall_ms: 1_760_000_000_000 + seq,
      drops_since_last: 0,
    };
    if (seq % 10 === 1) {
      if (call) calls.push(callRecord(call, callEffects));
      call = `c${seq}`;
      callEffects = [];
      events.push({
        ...base,
        process: null,
        body: {
          kind: "intent.tool_call.started",
          payload: {
            call_id: call,
            source: "hook",
            tool: "Bash",
            args: { command: `make step-${seq}` },
            turn: null,
          },
        },
      });
    } else {
      const denied = seq % 997 === 0;
      events.push({
        ...base,
        process: proc,
        body: denied
          ? {
              kind: "effect.net.connect",
              payload: {
                family: 2,
                addr: "203.0.113.9",
                port: 443,
                dns_name: null,
                verdict: "denied",
              },
            }
          : {
              kind: "effect.file.write",
              payload: {
                file: { path: `src/f${seq % 500}.ts`, in_cell: true, truncated: false },
                bytes: 10,
              },
            },
      });
      effects.push({
        seq,
        kind: "effect.file.write",
        ts_ns: seq * 1_000_000,
        pid: 4000,
        call_id: call,
        candidates: [],
        attribution: "window",
        confidence_bp: 8000,
      });
      callEffects.push(seq);
    }
  }
  if (call) calls.push(callRecord(call, callEffects));
  return { events, graph: { calls, processes: [], effects, agent_root: null } };
}

function callRecord(id: string, eff: number[]) {
  const started = Number(id.slice(1));
  return {
    call_id: id,
    tool: "Bash",
    source: "hook",
    started_seq: started,
    finished_seq: null,
    start_ns: 0,
    end_ns: 0,
    status: null,
    exit_code: null,
    command: "make",
    target_path: null,
    nested_in: null,
    effects: eff,
  };
}

/** Serve a synthetic session as session `perf` for the page. */
export async function serveSynthetic(page: Page, n: number) {
  const { events, graph } = syntheticSession(n);
  const record = {
    id: "perf",
    name: `synthetic ${n}`,
    workspace: "/w",
    command: ["agent"],
    mode: "cell",
    created_ms: 1,
    status: "closed",
    current_branch: "a",
    branches: [{ id: "a", name: "a", forked_from: null, created_ms: 1 }],
    checkpoints: [],
    cgroup: null,
    pid: null,
    runs: 1,
    agent: "synthetic",
    deny_tools: [],
    policy: null,
  };
  const json = (route: Route, body: unknown) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  await page.route("**/v1/**", (route) => {
    const url = new URL(route.request().url());
    const p = url.pathname;
    if (p === "/v1/sessions") return json(route, { sessions: [record] });
    if (p === "/v1/sessions/perf")
      return json(route, { ...record, head_seq: n, head_hash: "b3:00", sealed: true });
    if (p === "/v1/sessions/perf/graph") return json(route, graph);
    if (p === "/v1/sessions/perf/verify")
      return json(route, {
        session_id: "perf",
        events: n,
        head_hash: "b3:00",
        sealed: true,
        signer_fingerprint: null,
        problem: null,
        ok: true,
      });
    if (p === "/v1/sessions/perf/checkpoints")
      return json(route, { current_branch: "a", branches: record.branches, checkpoints: [] });
    if (p === "/v1/sessions/perf/events") {
      const after = Number(url.searchParams.get("after_seq") ?? "0");
      const limit = Number(url.searchParams.get("limit") ?? "500");
      const page = events.slice(after, after + limit);
      const last = page.at(-1)?.seq ?? after;
      return json(route, { events: page, next_after_seq: last < n ? last : null });
    }
    return route.fulfill({
      status: 404,
      contentType: "application/problem+json",
      body: '{"type":"not-found","title":"Not found","status":404}',
    });
  });
}

/** Intervals (ms) of `count` frames while the river is zoomed and panned (deadline 60 s). */
export async function measurePanning(page: Page, count = 150): Promise<number[]> {
  return page.evaluate(async (wanted) => {
    const canvas = document.querySelector("canvas");
    if (!canvas) throw new Error("no canvas");
    const r = canvas.getBoundingClientRect();
    const frames: number[] = [];
    let last = performance.now();
    const end = last + 60_000;
    let i = 0;
    await new Promise<void>((done) => {
      const tick = (t: number) => {
        frames.push(t - last);
        last = t;
        const zoom = i % 120 < 60;
        canvas.dispatchEvent(
          new WheelEvent("wheel", {
            deltaY: zoom ? -40 : 40,
            shiftKey: i % 3 === 0,
            clientX: r.left + r.width / 2,
            clientY: r.top + 40,
            bubbles: true,
          }),
        );
        i++;
        if (frames.length < wanted + 5 && t < end) requestAnimationFrame(tick);
        else done();
      };
      requestAnimationFrame(tick);
    });
    return frames.slice(5);
  }, count);
}

export function stats(frames: number[]) {
  const s = [...frames].sort((a, b) => a - b);
  const q = (p: number) => s[Math.min(s.length - 1, Math.floor(p * s.length))] ?? 0;
  return {
    frames: s.length,
    p50: q(0.5),
    p95: q(0.95),
    fps_p50: 1000 / q(0.5),
    fps_p95: 1000 / q(0.95),
  };
}
