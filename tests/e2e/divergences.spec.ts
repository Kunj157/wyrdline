import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";

import { expect, open, openSession, test } from "./fixtures";

async function seriousViolations(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

test("the inbox groups divergences across sessions by rule", async ({ page, consoleErrors }) => {
  await open(page, "/divergences");
  await expect(page.getByRole("heading", { level: 1, name: "Divergences" })).toBeVisible();
  const scope = page.getByRole("region", { name: /D002 out-of-scope-write/ });
  await expect(scope.getByRole("article")).toHaveCount(2);
  await expect(scope.getByRole("link", { name: "e2e greeting fix" })).toBeVisible();
  await expect(scope.getByRole("link", { name: "e2e live session" })).toBeVisible();
  const claim = page.getByRole("region", { name: /D001 claim-without-evidence/ });
  await expect(claim).toContainText("All tests pass.");
  expect(await seriousViolations(page)).toEqual([]);
  void consoleErrors;
});

test("evidence selects the cited event, and divergences are knots on the river", async ({
  page,
  consoleErrors,
}) => {
  await openSession(page);
  await page.getByRole("tab", { name: /^Divergences/ }).click();
  const item = page.getByRole("article", { name: /D002 out-of-scope-write/ });
  await expect(item).toContainText("wrote /home/user/.bashrc outside the workspace");
  await item.getByRole("button", { name: /^Select event #/ }).click();
  const inspector = page.getByRole("region", { name: "Inspector" });
  await expect(inspector).toContainText("/home/user/.bashrc");
  await inspector.getByRole("tab", { name: "Divergences (1)" }).click();
  await expect(inspector).toContainText("out-of-scope-write");
  await page.getByRole("tab", { name: "Events" }).click();
  await expect(page.getByRole("row", { selected: true })).toContainText("/home/user/.bashrc");
  // The SVG river exposes each knot's meaning; both divergences' evidence is marked.
  await page.getByRole("combobox", { name: /Renderer/ }).selectOption("svg");
  await expect(page.locator("g[data-meaning='divergence']")).toHaveCount(2);
  expect(await seriousViolations(page)).toEqual([]);
  void consoleErrors;
});

test("a divergence is acknowledged with a note", async ({ page, consoleErrors }) => {
  await openSession(page);
  await page.getByRole("tab", { name: /^Divergences/ }).click();
  const item = page.getByRole("article", { name: /D001 claim-without-evidence/ });
  await item.getByRole("button", { name: "Acknowledge" }).click();
  await item.getByLabel("Note").fill("no test run in this session");
  await item.getByRole("button", { name: "Save" }).click();
  await expect(item.locator("header [data-meaning]")).toHaveText("acknowledged");
  await expect(item).toContainText("Note: no test run in this session");
  // The inbox shows only open ones by default; the triage is in the ledger.
  await open(page, "/divergences");
  const group = page.getByRole("region", { name: /D002 out-of-scope-write/ });
  await expect(group.getByRole("article")).toHaveCount(2);
  await expect(page.getByRole("region", { name: /D001/ })).toHaveCount(0);
  await page.getByRole("button", { name: "All" }).click();
  await expect(page.getByRole("region", { name: /D001/ })).toContainText(
    "no test run in this session",
  );
  void consoleErrors;
});

test("a false positive is marked from the inbox", async ({ page, consoleErrors }) => {
  await open(page, "/divergences");
  const item = page
    .getByRole("article")
    .filter({ hasText: "wrote /home/user/.bashrc outside the workspace" });
  await item.getByRole("button", { name: "False positive" }).click();
  await item.getByLabel("Note").fill("the fixture writes it on purpose");
  await item.getByRole("button", { name: "Save" }).click();
  // Out of the open view, so the rule now has the live session's only.
  const group = page.getByRole("region", { name: /D002 out-of-scope-write/ });
  await expect(group.getByRole("article")).toHaveCount(1);
  await expect(group).toContainText("e2e live session");
  void consoleErrors;
});

test("a triage the daemon refuses while the session records changes nothing", async ({ page }) => {
  // The 409 itself is logged by the browser; anything else fails the test.
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/status of 409/.test(m.text())) errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(e.message));
  await open(page, "/divergences");
  const item = page.getByRole("article").filter({ hasText: "wrote /etc/hosts" });
  await expect(item.locator("header [data-meaning]")).toHaveText("open");
  await item.getByRole("button", { name: "Acknowledge" }).click();
  await item.getByLabel("Note").fill("seen it");
  await item.getByRole("button", { name: "Save" }).click();
  await expect(item.getByRole("alert")).toContainText("409");
  await expect(item.getByRole("alert")).toContainText("the session is recording");
  await expect(item.locator("header [data-meaning]")).toHaveText("open");
  await expect(item.getByLabel("Note")).toHaveValue("seen it");
  await page.reload();
  const again = page.getByRole("article").filter({ hasText: "wrote /etc/hosts" });
  await expect(again.locator("header [data-meaning]")).toHaveText("open");
  expect(errors).toEqual([]);
});
