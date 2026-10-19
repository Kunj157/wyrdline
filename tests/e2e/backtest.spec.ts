import { readFileSync } from "node:fs";
import { join } from "node:path";

import { env, expect, openSession, test } from "./fixtures";

/** Upload a policy over the API, backtest the fixture session from Studio. */
test("a backtest overlays what a policy would have stopped", async ({ page, consoleErrors }) => {
  const { base, token } = env();
  const wasm = readFileSync(join(__dirname, "../../policies/fixtures/hostile.wasm"));
  const up = await fetch(`${base}/v1/policies?name=hostile`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/wasm" },
    body: wasm,
  });
  expect(up.status).toBe(201);

  await openSession(page);
  await page.getByRole("tab", { name: "Backtest" }).click();
  await expect(page.getByRole("combobox", { name: "Policy" })).toHaveValue("hostile");
  await page.getByRole("button", { name: "Backtest", exact: true }).click();
  const status = page.getByRole("status").filter({ hasText: "would have stopped" });
  await expect(status).toContainText(/hostile would have stopped [1-9]\d* of \d+ events/);
  // The fixture's connect to 203.0.113.9 is outside the policy's egress list.
  const finding = status.getByRole("button", { name: /kernel deny.*203\.0\.113\.9/ });
  await expect(finding).toBeVisible();
  await finding.click();
  await page.getByRole("button", { name: "Clear overlay" }).click();
  await expect(status).toBeHidden();
  void consoleErrors;
});
