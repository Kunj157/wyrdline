import type { Browser, BrowserContext, Page, WebSocketRoute } from "@playwright/test";

import { BROWSER_NOISE, env, expect, test } from "../fixtures";

/**
 * Annotations of one bundle, synced through the Hub by three teammates in
 * three browser contexts (separate storage): two members and a viewer.
 */

interface Teammate {
  context: BrowserContext;
  page: Page;
  errors: string[];
  /** Cut the sync connection and refuse new ones until `online()`. */
  offline: () => Promise<void>;
  online: () => Promise<void>;
}

const secrets = (): Record<string, string> =>
  JSON.parse(process.env.WYRD_E2E_HUB_SECRETS ?? "{}") as Record<string, string>;

async function teammate(browser: Browser, who: string): Promise<Teammate> {
  const context = await browser.newContext();
  const { base, token, sid } = env();
  await context.addCookies([{ name: "wyrd_session", value: secrets()[who] ?? "", url: base }]);
  const page = await context.newPage();
  const errors: string[] = [];
  page.on("console", (m) => {
    if (
      (m.type() === "error" || m.type() === "warning") &&
      !BROWSER_NOISE.some((r) => r.test(m.text())) &&
      // The browser logs refused socket connections while offline.
      !/WebSocket connection to .* failed/.test(m.text())
    ) {
      errors.push(`${who} ${m.type()}: ${m.text()}`);
    }
  });
  page.on("pageerror", (e) => errors.push(`${who} pageerror: ${e.message}`));

  // Route the sync socket through the test so it can be cut on demand.
  let cut = false;
  const open: WebSocketRoute[] = [];
  await page.routeWebSocket(/\/api\/v1\/sync/, (ws) => {
    if (cut) {
      void ws.close();
      return;
    }
    ws.connectToServer();
    open.push(ws);
  });

  // The session view, with the bundle named in the page URL.
  const bundle = process.env.WYRD_E2E_HUB_BUNDLE ?? "";
  await page.goto(`${base}/sessions/${sid}?hub=${bundle}#token=${token}`);
  await page.getByRole("tab", { name: "Annotations" }).click();
  return {
    context,
    page,
    errors,
    offline: async () => {
      cut = true;
      await context.setOffline(true);
      await Promise.all(open.splice(0).map((ws) => ws.close()));
      await expect(status(page)).toContainText(/Offline|Reconnecting/);
    },
    online: async () => {
      cut = false;
      await context.setOffline(false);
      await expect(status(page)).toContainText("Synced with the Hub.", { timeout: 15_000 });
    },
  };
}

const status = (page: Page) =>
  page.getByRole("region", { name: "Annotations" }).getByRole("status");

const form = (page: Page) => page.getByRole("form", { name: "Add a comment" });

async function comment(page: Page, from: string, text: string) {
  const f = form(page);
  await f.getByLabel("From seq").fill(from);
  await f.getByLabel("To seq").fill(from);
  await f.getByLabel("Comment").fill(`${text} ${run}`);
  await f.getByRole("button", { name: "Add comment" }).click();
}

/** Tags this run's comments, so reruns against one database stay apart. */
const run = Math.random().toString(36).slice(2, 8);

const article = (page: Page, text: string) =>
  page.getByRole("article").filter({ hasText: `${text} ${run}` });

test("members sync one bundle's annotations live and across an offline period; a viewer reads", async ({
  browser,
}) => {
  // Two members first: the bundle gets its shared document from Alice, so
  // the viewer never sends the root change to an empty document (the Hub
  // refuses that as a write).
  const ana = await teammate(browser, "alice");
  const bo = await teammate(browser, "bob");
  await expect(status(ana.page)).toContainText("Synced with the Hub.");
  await expect(status(bo.page)).toContainText("Synced with the Hub.");

  // Live: Alice's comment reaches Bob without a reload.
  await comment(ana.page, "3", "alice: the .env read starts here");
  await expect(article(ana.page, "alice: the .env read starts here")).toBeVisible();
  await expect(article(bo.page, "alice: the .env read starts here")).toBeVisible();

  // The viewer joins after that, follows the document, and cannot add to it.
  const carol = await teammate(browser, "carol");
  await expect(status(carol.page)).toContainText("read-only");
  await expect(form(carol.page)).toHaveCount(0);
  await expect(article(carol.page, "alice: the .env read starts here")).toBeVisible();

  // Bob goes offline; both sides keep editing.
  await bo.offline();
  await comment(bo.page, "5", "bob: written offline");
  await comment(ana.page, "7", "alice: while bob is away");
  await expect(article(bo.page, "alice: while bob is away")).toHaveCount(0);
  await expect(article(ana.page, "bob: written offline")).toHaveCount(0);

  // Back online: the edits merge on both sides.
  await bo.online();
  for (const t of [ana, bo]) {
    await expect(article(t.page, "bob: written offline")).toBeVisible();
    await expect(article(t.page, "alice: while bob is away")).toBeVisible();
  }

  // The Hub holds the merged document: a fresh browser (no local copy)
  // as the viewer sees all three comments.
  const late = await teammate(browser, "carol");
  await expect(article(late.page, "alice: the .env read starts here")).toBeVisible();
  await expect(article(late.page, "bob: written offline")).toBeVisible();
  await expect(article(late.page, "alice: while bob is away")).toBeVisible();

  // Export: the saved document downloads under the session's name.
  const [download] = await Promise.all([
    ana.page.waitForEvent("download"),
    ana.page.getByRole("button", { name: "Download annotations" }).click(),
  ]);
  expect(download.suggestedFilename()).toMatch(/\.annotations\.automerge$/);

  expect([...ana.errors, ...bo.errors, ...carol.errors, ...late.errors], "console errors").toEqual(
    [],
  );
  for (const t of [ana, bo, carol, late]) await t.context.close();
});
