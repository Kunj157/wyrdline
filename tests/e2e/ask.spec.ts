import { spawn } from "node:child_process";

import { expect, open, test } from "./fixtures";

/** Run the fixture's live tool call that the policy asks about; resolves with its decision. */
function askingCall(): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.env.WYRD_E2E_FIXTURE ?? "", [
      "ask",
      process.env.WYRD_E2E_DATA ?? "",
      process.env.WYRD_E2E_LIVE_SID ?? "",
    ]);
    let out = "";
    child.stdout.on("data", (d: Buffer) => (out += d.toString()));
    child.on("error", reject);
    child.on("close", (code) => (code === 0 ? resolve(out) : reject(new Error(`exit ${code}`))));
  });
}

test("a pending ask is answered from the session screen", async ({ page, consoleErrors }) => {
  const live = process.env.WYRD_E2E_LIVE_SID ?? "";
  await open(page, `/sessions/${live}`);
  const decision = askingCall();
  const dialog = page.getByRole("dialog", { name: "Allow Bash?" });
  await expect(dialog).toBeVisible({ timeout: 10_000 });
  await expect(dialog.getByText("curl to a host outside the allow-list")).toBeVisible();
  await expect(dialog.getByText("diag.example/upload")).toBeVisible();
  await dialog.getByRole("button", { name: "Allow" }).click();
  await expect(dialog).toBeHidden();
  expect(JSON.parse(await decision)).toEqual({ type: "decision", decision: "allow", reason: null });
  void consoleErrors;
});
