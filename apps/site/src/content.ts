/**
 * Everything the page says, in one place.
 */
import type { Meaning } from "@wyrdline/ui";

export const repo = "https://github.com/Kunj157/wyrdline";

const base = import.meta.env.BASE_URL;
/** The real Studio, built into the site, replaying a recorded session. */
export const demoUrl = `${base}studio/?offline=${base}studio/demo-session.jsonl`;

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
