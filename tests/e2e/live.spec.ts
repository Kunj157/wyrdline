import { execFileSync } from "node:child_process";

import { expect, open, test } from "./fixtures";

/** Live mode: events appended to a recording session appear without a reload. */
test("a recording session streams new events into the open page", async ({
  page,
  consoleErrors,
}) => {
  const live = process.env.WYRD_E2E_LIVE_SID ?? "";
  await open(page, `/sessions/${live}`);
  await expect(page.getByText("recording", { exact: true })).toBeVisible();
  await page.getByRole("tab", { name: "Transcript" }).click();
  const text = `streamed while open ${Date.now()}`;
  execFileSync(process.env.WYRD_E2E_FIXTURE ?? "", [
    "append",
    process.env.WYRD_E2E_DATA ?? "",
    live,
    text,
  ]);
  await expect(page.getByText(text)).toBeVisible({ timeout: 10_000 });
  void consoleErrors;
});
