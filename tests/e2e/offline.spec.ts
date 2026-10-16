import { expect, test } from "./fixtures";

/**
 * Offline replay: Studio opens an exported session with no daemon calls and
 * computes the causal graph in the browser with the WebAssembly correlator.
 */
test("an exported session replays offline with the graph computed in the browser", async ({
  page,
  consoleErrors,
}) => {
  const base = process.env.WYRD_E2E_BASE ?? "";
  const apiCalls: string[] = [];
  page.on("request", (r) => {
    if (new URL(r.url()).pathname.startsWith("/v1/")) apiCalls.push(r.url());
  });
  await page.goto(`${base}/?offline=/e2e-offline.jsonl`);
  await expect(page.getByRole("status").filter({ hasText: "Offline replay" })).toBeVisible();
  await expect(page.getByText("chain not verifiable offline")).toBeVisible();
  // Effects attributed by the Wasm correlator show up in the session view.
  await expect(page.getByRole("region", { name: "Checkpoints and branches" })).toContainText(
    "Read-only replay",
  );
  await expect(page.getByRole("button", { name: /^Rewind to / })).toHaveCount(0);
  await expect(page.getByText(/Graph unavailable|not in this build/i)).toHaveCount(0);
  expect(apiCalls, "offline mode must not call the daemon").toEqual([]);
  // The fixture fails the test on any console error or warning.
  void consoleErrors;
});
