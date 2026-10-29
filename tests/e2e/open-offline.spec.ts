import { type ChildProcess, spawn } from "node:child_process";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { expect, test } from "./fixtures";

/**
 * `wyrd open` serves Studio in bundle mode with no daemon; the service
 * worker caches Studio on that first visit. Then the server is stopped and
 * the browser goes offline: Studio still loads, opens the bundle from the
 * file picker (verified in the browser), and the whole Session screen
 * works; a tampered bundle is refused.
 */
const studio = () => process.env.WYRD_E2E_STUDIO ?? "";

function wyrdOpen(bundle: string): Promise<{ server: ChildProcess; url: string }> {
  const server = spawn(
    process.env.WYRD_E2E_WYRD ?? "wyrd",
    ["open", "--print", "--studio", studio(), bundle],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  return new Promise((ok, fail) => {
    server.once("error", fail);
    server.once("exit", (code) => fail(new Error(`wyrd open exited with ${code}`)));
    createInterface({ input: server.stdout! }).once("line", (url) => ok({ server, url }));
  });
}

test("Studio opens a bundle with no network and no daemon after the first visit", async ({
  page,
  context,
}) => {
  const { server, url } = await wyrdOpen(join(studio(), "e2e.wyrd"));
  try {
    expect(url).toMatch(/^http:\/\/127\.0\.0\.1:\d+\/\?bundle=\/bundle\/e2e\.wyrd$/);
    // First visit, served by `wyrd open`: the bundle opens verified.
    await page.goto(url);
    await expect(page.getByRole("heading", { name: "e2e greeting fix" })).toBeVisible();
    await expect(page.getByText("chain intact, signed")).toBeVisible();
    await page.evaluate(() => navigator.serviceWorker.ready.then(() => undefined));
  } finally {
    server.kill("SIGTERM");
  }

  // No server, no network.
  await context.setOffline(true);
  const origin = new URL(url).origin;
  await page.goto(`${origin}/`);
  const input = page.getByLabel("Open a bundle or session file");
  await input.setInputFiles(join(studio(), "e2e-tampered.wyrd"));
  await expect(page.getByRole("alert")).toContainText("did not verify");
  await expect(page.getByRole("application", { name: /Causal river/ })).toHaveCount(0);

  await input.setInputFiles(join(studio(), "e2e.wyrd"));
  await expect(
    page.getByRole("status").filter({ hasText: "verified in your browser" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "e2e greeting fix" })).toBeVisible();
  await expect(page.getByText("chain intact, signed")).toBeVisible();
  // The full Session screen: river (correlated in WebAssembly), events,
  // divergences, transcript, checkpoints, and file diffs from cas/.
  await expect(page.getByRole("application", { name: /Causal river/ })).toBeVisible();
  await expect(page.getByText(/Graph unavailable|not in this build/i)).toHaveCount(0);
  await expect(page.getByRole("table", { name: "Session events in order" })).toBeVisible();
  await page.getByRole("tab", { name: /Divergences/ }).click();
  await expect(page.getByRole("tabpanel")).toContainText("D001");
  await page.getByRole("tab", { name: "Transcript" }).click();
  await expect(page.getByText("All tests pass.")).toBeVisible();
  await page.getByRole("button", { name: "Diff c4" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: /greet\.txt/ }).click();
  await expect(dialog.getByLabel("Line diff")).toContainText("+ hello, world");
  await context.setOffline(false);
});
