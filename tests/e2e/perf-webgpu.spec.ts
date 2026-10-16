import { writeFileSync } from "node:fs";

import { open, test } from "./fixtures";
import { measurePanning, serveSynthetic, stats } from "./synthetic";

// Runs only in the webgpu project (headed, SwiftShader under Xvfb). A
// software GPU is not the plan's laptop iGPU, so numbers are recorded,
// not asserted.
test("river performance with 200 000 events (WebGPU)", async ({ page }, info) => {
  test.setTimeout(240_000);
  await serveSynthetic(page, 200_000);
  const start = Date.now();
  await open(page, "/sessions/perf");
  await page.locator("[data-renderer='webgpu']").waitFor({ timeout: 180_000 });
  const firstPaintMs = Date.now() - start;
  const s = stats(await measurePanning(page));
  writeFileSync(
    info.outputPath("perf-webgpu-200000.json"),
    JSON.stringify(
      { events: 200_000, renderer: "webgpu-swiftshader", firstPaintMs, ...s },
      null,
      2,
    ),
  );
});
