import { readFileSync } from "node:fs";
import { join } from "node:path";

import type { Page } from "@playwright/test";

import { env, expect, open, test } from "./fixtures";

/**
 * Bundle mode: a `.wyrd` bundle (global-setup exports the fixture session)
 * is verified in the browser by the WebAssembly reader and opened without
 * any daemon call; a tampered copy is refused with the failing check and
 * nothing of it is rendered.
 */
const studio = () => process.env.WYRD_E2E_STUDIO ?? "";

function noDaemonCalls(page: Page): string[] {
  const calls: string[] = [];
  page.on("request", (r) => {
    if (new URL(r.url()).pathname.startsWith("/v1/")) calls.push(r.url());
  });
  return calls;
}

async function expectVerifiedSession(page: Page) {
  await expect(
    page.getByRole("status").filter({ hasText: "verified in your browser" }),
  ).toContainText("Bundle e2e.wyrd");
  await expect(page.getByRole("heading", { name: "e2e greeting fix" })).toBeVisible();
  await expect(page.getByText("chain intact, signed")).toBeVisible();
  await expect(page.getByRole("application", { name: /Causal river/ })).toBeVisible();
}

test("a bundle opened by URL (wyrd open) verifies and renders without the daemon", async ({
  page,
  consoleErrors,
}) => {
  const calls = noDaemonCalls(page);
  await page.goto(`${env().base}/?bundle=/e2e.wyrd`);
  await expectVerifiedSession(page);
  const panel = page.getByRole("region", { name: "Checkpoints and branches" });
  await expect(panel).toContainText("Read-only bundle");
  await expect(page.getByRole("button", { name: /^Rewind to / })).toHaveCount(0);
  // Diffs and file contents come from the bundle's cas/.
  await page.getByRole("button", { name: "Diff c4" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: /greet\.txt/ }).click();
  await expect(dialog.getByLabel("Line diff")).toContainText("+ hello, world");
  await expect(dialog.getByLabel("Line diff")).toContainText("− hello world");
  await dialog.getByRole("button", { name: "Close" }).click();
  // Divergences and the transcript come from the verified events.
  await page.getByRole("tab", { name: "Transcript" }).click();
  await expect(page.getByText("All tests pass.")).toBeVisible();
  expect(calls, "bundle mode must not call the daemon").toEqual([]);
  void consoleErrors;
});

test("a tampered bundle names the failing check and renders nothing", async ({ page }) => {
  await page.goto(`${env().base}/?bundle=/e2e-tampered.wyrd`);
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("The bundle did not verify");
  await expect(alert.getByRole("list", { name: "Bundle checks" })).toContainText("✗");
  await expect(page.getByRole("application", { name: /Causal river/ })).toHaveCount(0);
  await expect(page.getByText("e2e greeting fix")).toHaveCount(0);
});

test("the file picker opens a bundle and refuses a tampered one", async ({ page }) => {
  await open(page, "/");
  const input = page.getByLabel("Open a bundle or session file");
  await input.setInputFiles(join(studio(), "e2e-tampered.wyrd"));
  await expect(page.getByRole("alert")).toContainText("did not verify");
  await expect(page).toHaveURL(/\/(#.*)?$/);
  await input.setInputFiles(join(studio(), "e2e.wyrd"));
  await expectVerifiedSession(page);
});

test("a bundle dropped on Studio opens", async ({ page }) => {
  await open(page, "/");
  const bytes = [...readFileSync(join(studio(), "e2e.wyrd"))];
  const drop = await page.evaluateHandle((data) => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array(data)], "e2e.wyrd"));
    return dt;
  }, bytes);
  const zone = page.getByRole("region", { name: "Drop a bundle or session file" });
  await zone.dispatchEvent("dragover", { dataTransfer: drop });
  await zone.dispatchEvent("drop", { dataTransfer: drop });
  await expectVerifiedSession(page);
});
