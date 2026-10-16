# Studio river performance — Phase 8

Measured with the Playwright specs in `tests/e2e/perf*.spec.ts`: a
synthetic session (every tenth event starts a tool call, the rest are file
writes attributed to it, about 0.1 % denied connects) is served by
intercepting the daemon API. "First paint" is from navigation to the first
drawn river, including fetching every event page and the graph. Frame
intervals are sampled with `requestAnimationFrame` over 150 frames while
the river is continuously zoomed and panned (one wheel event per frame).

Workstation: Ubuntu 24.04, 48 cores, shared (load average ~17 during the
runs), Chromium 1243 from Playwright 1.63.

| Events | Renderer | First paint | Frame p50 | Frame p95 | fps (p95) |
|---|---|---|---|---|---|
| 20 000 | Canvas2D (headless) | 0.8 s | 16.7 ms | 16.7 ms | 60 |
| 200 000 | Canvas2D (headless) | 5.0 s | 16.7 ms | 16.8 ms | 60 |
| 200 000 | WebGPU on SwiftShader (headed, Xvfb) | 4.6 s | 16.7 ms | 33.2 ms | 30 |

Targets (plan §16.5): **≥ 30 fps at 20 000 events on Canvas2D — met** and
asserted in CI. **60 fps at 200 000 events on a laptop iGPU with WebGPU —
not measured here**: there is no GPU on the workstation or the CI runners,
and SwiftShader emulates one on the CPU; even so it holds 60 fps at the
median. The number on real hardware belongs in the release evidence.

How it stays fast: typed arrays for knots and threads, binary search for
the visible range, threads culled to those ending in view, level-of-detail
buckets above 2 000 visible knots per lane (only the selected call's
threads drawn), drawing only on change, device pixel ratio capped at 2.
Culling threads took WebGPU-on-SwiftShader at 200 000 events from 0.6 fps
to the numbers above.

First paint at 200 000 events (≈ 4–5 s) is dominated by fetching 40
JSON pages and the graph; the 3 s budget in §16.5 is for bundle mode
(Phase 11), where the correlator runs in the browser on a local file.
