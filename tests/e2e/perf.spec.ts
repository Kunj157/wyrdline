import { writeFileSync } from "node:fs";

import { expect, open, test } from "./fixtures";
import { measurePanning, serveSynthetic, stats } from "./synthetic";

const results: Record<string, unknown> = {};

for (const n of [20_000, 200_000]) {
  test(`river performance with ${n} events (Canvas2D)`, async ({ page }, info) => {
    test.setTimeout(240_000);
    await serveSynthetic(page, n);
    const start = Date.now();
    await open(page, "/sessions/perf");
    await page.locator("[data-renderer='canvas2d']").waitFor({ timeout: 180_000 });
    await page.waitForFunction(() => (document.querySelector("canvas")?.width ?? 0) > 0);
    const firstPaintMs = Date.now() - start;
    const s = stats(await measurePanning(page));
    results[`${info.project.name}-${n}`] = { events: n, renderer: "canvas2d", firstPaintMs, ...s };
    writeFileSync(
      info.outputPath(`perf-${n}.json`),
      JSON.stringify(results[`${info.project.name}-${n}`], null, 2),
    );
    // Plan §16.5: at least 30 fps at 20 000 events on Canvas2D.
    if (n === 20_000) expect(s.fps_p95, JSON.stringify(s)).toBeGreaterThanOrEqual(30);
  });
}
