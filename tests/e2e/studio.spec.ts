import AxeBuilder from "@axe-core/playwright";

import { env, expect, open, openSession, test } from "./fixtures";

test("the sessions list opens a session and the token leaves the address bar", async ({
  page,
  consoleErrors,
}) => {
  await open(page);
  expect(new URL(page.url()).hash).toBe("");
  const link = page.getByRole("link", { name: "e2e greeting fix" });
  await expect(link).toBeVisible();
  await link.click();
  await page.waitForURL((u) => u.pathname === `/sessions/${env().sid}`);
  await expect(page.getByText("chain intact")).toBeVisible();
  void consoleErrors;
});

test("the river, the table, and the inspector move together", async ({ page, consoleErrors }) => {
  await openSession(page);
  const river = page.getByRole("application", { name: /Causal river/ });
  await river.focus();
  await page.keyboard.press("Home");
  const first = await page.getByRole("row", { selected: true }).textContent();
  await page.keyboard.press("ArrowRight");
  const second = await page.getByRole("row", { selected: true }).textContent();
  expect(second).not.toBe(first);
  // n jumps to the next notable knot (denied, irreversible, divergence).
  await page.keyboard.press("Home");
  await page.keyboard.press("n");
  const inspector = page.getByRole("region", { name: "Inspector" });
  await expect(
    inspector.locator("[data-meaning='denied'], [data-meaning='irreversible']").first(),
  ).toBeVisible();
  void consoleErrors;
});

test("a tool call shows what it caused", async ({ page, consoleErrors }) => {
  await openSession(page);
  // The call is near the end of the session; the table is virtualized.
  await page.getByRole("table", { name: "Session events in order" }).evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await page.getByRole("button", { name: /^Bash curl https:\/\/diag\.example\/upload/ }).click();
  const inspector = page.getByRole("region", { name: "Inspector" });
  await inspector.getByRole("tab", { name: "Attribution" }).click();
  await expect(inspector.getByRole("tabpanel")).toContainText(/Tool call .*caused/);
  void consoleErrors;
});

test("the transcript lists the turns", async ({ page, consoleErrors }) => {
  await openSession(page);
  await page.getByRole("tab", { name: "Transcript" }).click();
  await expect(page.getByText("All tests pass.")).toBeVisible();
  void consoleErrors;
});

test("a file diff between checkpoints", async ({ page, consoleErrors }) => {
  await openSession(page);
  await page.getByRole("button", { name: "Diff c4" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: /greet\.txt/ }).click();
  await expect(dialog.getByLabel("Line diff")).toContainText("+ hello, world");
  await expect(dialog.getByLabel("Line diff")).toContainText("− hello world");
  await dialog.getByRole("button", { name: "Close" }).click();
  void consoleErrors;
});

test("rewind lists what cannot be undone, then creates a branch", async ({
  page,
  consoleErrors,
}) => {
  await openSession(page);
  await page.getByRole("button", { name: "Rewind to c3" }).click();
  const dialog = page.getByRole("dialog", { name: "Rewind to c3" });
  await expect(dialog).toContainText("cannot be undone");
  await expect(dialog).toContainText("/home/user/.bashrc");
  await dialog.getByRole("button", { name: "Rewind", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Rewound: branch");
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(page.getByRole("region", { name: "Checkpoints and branches" })).toContainText(
    "branch b",
  );
  void consoleErrors;
});

test("fork validates the branch name", async ({ page, consoleErrors }) => {
  await openSession(page);
  await page.getByRole("button", { name: "Fork from c2" }).click();
  const dialog = page.getByRole("dialog", { name: "Fork from c2" });
  const name = dialog.getByLabel("Branch name");
  await name.fill("bad name!");
  await expect(dialog.getByRole("button", { name: "Fork", exact: true })).toBeDisabled();
  await name.fill("careful-retry");
  await dialog.getByRole("button", { name: "Fork", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Forked branch careful-retry");
  void consoleErrors;
});

test("apply previews before writing the workspace", async ({ page, consoleErrors }) => {
  await openSession(page);
  await page.getByRole("button", { name: "Apply c4" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "Apply", exact: true })).toBeDisabled();
  await dialog.getByRole("button", { name: "Preview" }).click();
  await expect(dialog.getByRole("list", { name: "Planned changes" })).toContainText("greet.txt");
  await dialog.getByRole("button", { name: "Apply", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Applied");
  void consoleErrors;
});

test("no serious accessibility violations", async ({ page }) => {
  await openSession(page);
  for (const path of ["session", "sessions"]) {
    if (path === "sessions") await open(page);
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(
      serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`),
    ).toEqual([]);
  }
});
