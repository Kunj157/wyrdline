# Attribution evaluation — Phase 6

`cargo nextest run -p wyrd-correlator` (test `eval`), 300 generated
sessions with ground truth known by construction (`wyrd_correlator::synth`):
Bash calls with process subtrees and background processes — half of them
wrapped the way Claude Code's Bash tool wraps commands, so the command is
not visible in the 63 bytes of argv the kernel keeps — in-process edits,
MCP calls served by a long-lived server with nested gateway calls,
overlapping calls, agent-runtime noise, unrelated processes, PID reuse.

| Metric | Result | Target (plan §18.2) |
|---|---|---|
| Labelled effects | 13 024 | |
| Precision (attributed effects correct) | **1.0000** (11 105 / 11 105) | ≥ 0.98 |
| Recall (effects with a cause found) | **1.0000** (11 105 / 11 105) | ≥ 0.95 |
| Ambiguous-by-construction effects flagged, not guessed | **420 / 420** | all |
| Throughput (release) | **2.0 M events/s** | ≥ 100 k |
| Native vs Wasm output | **40 / 40 identical** | identical |

Before the window-subtree rule, wrapped commands lost their background
processes: recall was **0.9828** on the same set.

**Caveat:** synthetic sessions encode the patterns the rules were written
for, so these numbers are an upper bound. On a real recorded session (the
`basic` fakeagent scenario under `wyrd run` with the live sensor, 105
effects) every file effect was attributed to the right call; this run
also found that Wyrdline's own `wyrd hook` helpers were being counted as
agent activity, now classified as `infrastructure`. Labelled real
sessions from live agents are added to the eval set in Phase 10.
