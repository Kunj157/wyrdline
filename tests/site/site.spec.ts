import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const home = "/wyrdline/";

function watchConsole(page: Page) {
  const problems: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" || m.type() === "warning") problems.push(`${m.type()}: ${m.text()}`);
  });
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  return problems;
}

test("renders every section with no console errors", async ({ page }) => {
  const problems = watchConsole(page);
  await page.goto(home);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Every agent leaves a thread.");
  for (const name of [
    "What an agent says it did and what it actually did can differ.",
    "Record, correlate, rewind.",
    "The real Studio, in your browser.",
    "Where the thread goes next.",
    "Build it from source",
  ]) {
    await expect(page.getByRole("heading", { level: 2, name })).toBeVisible();
  }
  expect(problems).toEqual([]);
});

test.describe("accessibility", () => {
  // Under reduced motion every section is in its final state (nothing
  // mid-fade), so axe checks the colours people actually read.
  test.use({ reducedMotion: "reduce" });

  test("passes axe (WCAG 2.1 AA)", async ({ page }) => {
    await page.goto(home);
    await page.waitForLoadState("networkidle");
    const axe = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(axe.violations.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual([]);
  });
});

for (const width of [390, 768, 1024, 1440]) {
  test(`has no horizontal scroll at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(home);
    await page.waitForLoadState("networkidle");
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
  });
}

test.describe("reduced motion", () => {
  test.use({ reducedMotion: "reduce" });

  test("keeps the static weave and never fetches the 3D chunk", async ({ page }) => {
    const chunks: string[] = [];
    page.on("request", (r) => r.url().includes("LoomCanvas") && chunks.push(r.url()));
    await page.goto(home);
    await page.waitForLoadState("networkidle");
    await expect(page.getByRole("img", { name: /threads from tool calls/i })).toBeVisible();
    await expect(page.locator("canvas")).toHaveCount(0);
    expect(chunks).toEqual([]);
    const animation = await page
      .locator(".wyrd-marquee")
      .evaluate((el) => getComputedStyle(el).animationName);
    expect(animation).toBe("none");
  });
});

test("lazy-loads the 3D Loom when WebGL is available", async ({ page }) => {
  await page.goto(home);
  const webgl = await page.evaluate(() => !!document.createElement("canvas").getContext("webgl2"));
  test.skip(!webgl, "no WebGL2 in this browser");
  await expect(page.locator('[data-3d="on"]')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("loom-3d")).toHaveCSS("opacity", "1", { timeout: 10_000 });
});

test("opens the demo: the real Studio replays the recorded session offline", async ({ page }) => {
  const problems = watchConsole(page);
  const api: string[] = [];
  page.on("request", (r) => new URL(r.url()).pathname.includes("/v1/") && api.push(r.url()));
  await page.goto(home);
  await page.getByRole("link", { name: "Open the live demo" }).first().click();
  await expect(page).toHaveURL(/\/wyrdline\/studio\/sessions\//);
  await expect(page.getByRole("status").filter({ hasText: "Offline replay" })).toBeVisible();
  await expect(page.getByText(/122 knots/)).toBeVisible({ timeout: 15_000 });
  expect(api).toEqual([]);
  // Studio's own "no WebGPU adapter" notice is expected in headless Chromium.
  expect(problems.filter((p) => !p.includes("No available adapters"))).toEqual([]);
});
