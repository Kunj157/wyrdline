import { spawn } from "node:child_process";

import { expect, open, test } from "./fixtures";

const sid = () => process.env.WYRD_E2E_REWIND_SID ?? "";

/**
 * Ask for a rewind the way an agent does: `wyrd mcp` over stdio, the
 * `request_rewind` tool. Resolves with the tool result.
 */
function requestRewind(checkpoint: string, reason: string): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.env.WYRD_E2E_WYRD ?? "", ["mcp", "--session", sid()], {
      env: { ...process.env, WYRD_DATA_DIR: process.env.WYRD_E2E_DATA ?? "" },
    });
    let out = "";
    child.stdout.on("data", (d: Buffer) => {
      out += d.toString();
      const lines = out.split("\n").filter(Boolean);
      const answer = lines
        .map((l) => JSON.parse(l) as { id?: number; result?: Record<string, unknown> })
        .find((m) => m.id === 2);
      if (answer?.result) {
        child.kill();
        resolve(answer.result);
      }
    });
    child.on("error", reject);
    const send = (m: unknown) => child.stdin.write(`${JSON.stringify(m)}\n`);
    send({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "e2e", version: "1" },
      },
    });
    send({ jsonrpc: "2.0", method: "notifications/initialized" });
    send({
      jsonrpc: "2.0",
      id: 2,
      method: "tools/call",
      params: { name: "request_rewind", arguments: { checkpoint_id: checkpoint, reason } },
    });
  });
}

test("the agent's rewind requests wait for the user, who rejects or confirms them", async ({
  page,
  consoleErrors,
}) => {
  const first = await requestRewind("c1", "my edit to notes.txt was wrong");
  expect(first.isError).toBe(false);
  await open(page, `/sessions/${sid()}`);
  const requests = page.getByRole("region", { name: "Rewind requests" });
  await expect(requests.getByText("my edit to notes.txt was wrong")).toBeVisible();
  // Nothing happened yet: one branch, still current.
  const tree = page.getByRole("region", { name: "Checkpoints and branches" });
  await expect(tree.getByText("branch a")).toBeVisible();
  await expect(tree.getByText("branch b")).toHaveCount(0);

  await requests.getByRole("button", { name: /^Reject rewind request/ }).click();
  await expect(page.getByRole("status").getByText(/nothing was rewound/)).toBeVisible();
  await expect(tree.getByText("branch b")).toHaveCount(0);

  await requestRewind("c1", "please start over from c1");
  await page.reload();
  await expect(requests.getByText("please start over from c1")).toBeVisible();
  await requests.getByRole("button", { name: /^Review rewind request/ }).click();
  const dialog = page.getByRole("dialog", { name: "Rewind to c1?" });
  await expect(dialog.getByText("please start over from c1")).toBeVisible();
  // Cancelling decides nothing.
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  await expect(tree.getByText("branch b")).toHaveCount(0);

  await requests.getByRole("button", { name: /^Review rewind request/ }).click();
  await dialog.getByRole("button", { name: "Confirm rewind" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status").getByText(/now on branch b/)).toBeVisible();
  await expect(tree.getByText("branch b")).toBeVisible();
  void consoleErrors;
});
