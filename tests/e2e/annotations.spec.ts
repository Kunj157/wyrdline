import AxeBuilder from "@axe-core/playwright";

import { expect, openSession, test } from "./fixtures";

/**
 * Annotations are kept on this device (IndexedDB) and survive a reload;
 * the Automerge wasm module loads under the daemon's CSP.
 */
test("a comment anchored to the selected event survives a reload", async ({
  page,
  consoleErrors,
}) => {
  await openSession(page);
  await page.getByRole("tab", { name: "Events" }).click();
  const row = page.getByRole("row").nth(3);
  await row.click();
  const seq = (await row.getByRole("cell").first().innerText()).replace(/\D/g, "");
  await page.getByRole("tab", { name: "Annotations" }).click();
  const form = page.getByRole("form", { name: "Add a comment" });
  await expect(form.getByLabel("From seq")).toHaveValue(seq);
  await form.getByLabel("Your name").fill("ana");
  await form.getByLabel("Comment").fill("this is where it went wrong");
  await form.getByRole("button", { name: "Add comment" }).click();
  const comment = page.getByRole("article", { name: `Comment by ana on seq ${seq}` });
  await expect(comment).toContainText("this is where it went wrong");
  await page
    .getByRole("checkbox", { name: /^Pin checkpoint/ })
    .first()
    .check();
  // The annotations tab itself (the inspector is covered by its own specs).
  const results = await new AxeBuilder({ page })
    .include('section[aria-label="Annotations"]')
    .analyze();
  expect(
    results.violations.filter((v) => v.impact === "serious" || v.impact === "critical"),
  ).toEqual([]);

  // Saved locally after a short batch delay; then reload the page.
  await expect(page.getByRole("status").filter({ hasText: "Saved on this device" })).toBeVisible();
  await page.reload();
  await page.getByRole("tab", { name: "Annotations" }).click();
  await expect(page.getByRole("article", { name: `Comment by ana on seq ${seq}` })).toBeVisible();
  await expect(page.getByRole("checkbox", { name: /^Pin checkpoint/ }).first()).toBeChecked();
  void consoleErrors;
});
