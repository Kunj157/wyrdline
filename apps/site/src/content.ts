/**
 * Everything the page says, in one place. Shipped features describe what is
 * merged and released; the roadmap lists the phases still to come, and each
 * one moves into `features` (with a screenshot or recording) as it ships.
 */
import type { Meaning } from "@wyrdline/ui";

export const repo = "https://github.com/Kunj157/wyrdline";

const base = import.meta.env.BASE_URL;
/** The real Studio, built into the site, replaying a recorded session. */
export const demoUrl = `${base}studio/?offline=${base}studio/demo-session.jsonl`;
export const shot = (name: string) => `${base}shots/${name}`;

export interface Plane {
  meaning: Meaning;
  name: string;
  source: string;
  example: string;
}

export const planes: Plane[] = [
  {
    meaning: "intent",
    name: "Intent",
    source: "Agent hooks, the MCP gateway, OpenTelemetry GenAI spans",
    example: 'Bash("rm old.txt && mkdir -p out && …")',
  },
  {
    meaning: "effect",
    name: "Effect",
    source: "eBPF in the kernel, scoped to the agent's cgroup",
    example: "exec rm · unlink old.txt · open_write out/env.txt",
  },
  {
    meaning: "ok",
    name: "State",
    source: "A content-addressed checkpoint after every tool call",
    example: "checkpoint c4 = manifest b3:9f2e…",
  },
];

export interface Feature {
  id: string;
  phase: string;
  title: string;
  body: string;
  points: string[];
  shot?: { src: string; alt: string };
}

export const features: Feature[] = [
  {
    id: "ledger",
    phase: "Phase 2",
    title: "A ledger you can check",
    body: "Intent, effect and state land in one hash-chained ledger. Every row carries the hash of the one before it and the head is signed when the session closes, so editing history shows up at the exact sequence number.",
    points: [
      "BLAKE3 hash chain, ed25519-signed session head",
      "Append-only: the database rejects updates and deletes",
      "Secrets redacted before anything is stored",
    ],
  },
  {
    id: "cell",
    phase: "Phase 3",
    title: "The Cell: rewind anything, not just the agent's own edits",
    body: "The agent runs in a cgroup-scoped, copy-on-write sandbox. Every tool call ends in a checkpoint, so a shell script's deletes, a build's outputs and a package manager's writes rewind with it.",
    points: [
      "Checkpoint, rewind, fork, diff, and apply between branches",
      "overlayfs upper layers deduplicated into a BLAKE3 content store",
      "Unprivileged user namespaces and Landlock rules",
    ],
    shot: {
      src: shot("checkpoints.jpg"),
      alt: "Studio: the transcript next to the session's checkpoints c1 to c8 on branch a",
    },
  },
  {
    id: "daemon",
    phase: "Phases 4 and 7",
    title: "Works with the agents you already use",
    body: "Hooks for the claude-code, Codex and Gemini CLIs, an MCP gateway for any MCP client, and an OTLP receiver for agents that emit GenAI traces. The per-user daemon serves an authenticated API and a live stream to Studio and the CLI.",
    points: [
      "wyrd run, events, graph, explain, tail, rewind, fork, apply",
      "Same-UID socket plus a token-guarded loopback port",
      "OpenAPI contract with generated TypeScript types",
    ],
  },
  {
    id: "sensor",
    phase: "Phase 5",
    title: "The kernel's side of the story",
    body: "An eBPF sensor follows the agent's whole process tree, including every subprocess it spawns, and records what really ran, what was written, renamed and deleted, and where it connected.",
    points: [
      "exec, fork, exit, file writes, renames, unlinks, connects",
      "Scoped to the session's cgroup; dropped events are counted in the ledger",
      "Under 2 % overhead on a cargo build",
    ],
  },
  {
    id: "correlator",
    phase: "Phase 6",
    title: "Every effect, traced to the tool call that caused it",
    body: "The correlator attributes each kernel effect to a tool call, with a cause and a confidence, by walking the process tree and the call's time window. It is a pure Rust function, so the same code runs in the daemon and, as WebAssembly, in your browser.",
    points: [
      "direct, window, ambiguous, agent-runtime, infrastructure, unattributed",
      "Native and Wasm outputs checked identical in CI",
      "Explain any effect: process chain, cause, confidence",
    ],
    shot: {
      src: shot("attribution.jpg"),
      alt: "Studio: exec rm old.txt selected, attributed directly with 100% confidence to the Bash tool call that removed the file",
    },
  },
  {
    id: "studio",
    phase: "Phase 8",
    title: "Studio: follow the thread",
    body: "The Causal River draws intent as threads from the top lane to the kernel effects they caused below, with checkpoints on the spine. Pick any knot to see the raw event, its attribution and the policy decision, live while the agent works or replayed from a file.",
    points: [
      "WebGPU river with Canvas2D and SVG fallbacks, keyboard navigable",
      "Live mode over a resumable event stream",
      "Offline replay with the in-browser correlator, as in the demo",
    ],
    shot: {
      src: shot("inspect.jpg"),
      alt: "Studio: the Causal River with one Bash tool call selected and its six kernel effects highlighted, the inspector showing the call's payload",
    },
  },
];

export interface Phase {
  n: number;
  title: string;
  body: string;
}

/** Still to come; each moves into `features` as it ships. */
export const roadmap: Phase[] = [
  {
    n: 9,
    title: "Guard",
    body: "Policies as WebAssembly components, enforced in the kernel with Landlock and cgroup BPF, and backtested against recorded sessions.",
  },
  {
    n: 10,
    title: "Divergence",
    body: "Flag claims the effects contradict: “all tests pass” with no test run, writes outside the task, a secret read and then a connection.",
  },
  {
    n: 11,
    title: "Bundles",
    body: "Redacted, signed .wyrd bundles that replay offline in the browser, and an MCP server so agents can query their own history.",
  },
  {
    n: 12,
    title: "Hub",
    body: "Local-first annotations shared with teammates, and a team Hub for sessions across machines.",
  },
  {
    n: 13,
    title: "CI capture",
    body: "A GitHub Action that records agent runs in CI, plus containers and Kubernetes.",
  },
  {
    n: 14,
    title: "Launch",
    body: "Hardening, performance, signed releases, install packages, and the demo video.",
  },
];

export const stack = [
  "Rust",
  "Aya eBPF",
  "Landlock",
  "overlayfs",
  "cgroup v2",
  "BLAKE3",
  "SQLite",
  "Axum",
  "MCP",
  "OpenTelemetry",
  "WebAssembly",
  "React 19",
  "WebGPU",
  "Tailwind v4",
];
