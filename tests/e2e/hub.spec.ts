import AxeBuilder from "@axe-core/playwright";
import type { Page, Route } from "@playwright/test";

import { expect, open, test } from "./fixtures";

/**
 * Hub mode: the dashboard and search pages against a stand-in Hub API
 * (the real Hub's answers are covered by the wyrd-hub container tests;
 * these shapes follow docs/architecture/hub.md).
 */
const me = {
  id: "u1",
  email: "a@example.test",
  name: "A",
  orgs: [
    { slug: "acme", name: "Acme", role: "viewer" },
    { slug: "beta", name: "Beta", role: "admin" },
  ],
};

const stats = (org: string) => ({
  sessions: {
    total: org === "acme" ? 3 : 1,
    considered: 3,
    by_repo: [
      { repo: "shop", sessions: 2 },
      { repo: "auth", sessions: 1 },
    ],
    by_agent: [{ agent: "claude-code", sessions: 3 }],
  },
  divergences: {
    by_rule: [
      { rule: "D001", sessions: 2, raised: 2, rate: 2 / 3 },
      { rule: "D003", sessions: 1, raised: 1, rate: 1 / 3 },
    ],
  },
  denied: {
    total: 3,
    by_kind: [
      { kind: "effect.net.connect", count: 1 },
      { kind: "guard.decision", count: 2 },
    ],
  },
  capture: { events: 120, dropped_events: 7, sensor_lost: 1, overhead_pct: null },
  judge: { recorded: false, runs: 0, input_tokens: 0, output_tokens: 0, cost_usd: 0 },
});

const json = (route: Route, status: number, body: unknown) =>
  route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });

async function hub(page: Page, signedIn = true) {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (!signedIn) return json(route, 401, { title: "Sign in or send a valid API token" });
    if (path === "/api/v1/me") return json(route, 200, me);
    const m = /^\/api\/v1\/orgs\/([a-z]+)\/(stats|search)$/.exec(path);
    if (m?.[2] === "stats") return json(route, 200, stats(m[1] ?? ""));
    if (m?.[2] === "search") {
      const q = url.searchParams.get("q") ?? "";
      if (q.includes("colour:")) {
        return json(route, 400, {
          title: "Bad request",
          detail: 'unknown filter "colour" at position 10',
          position: 10,
        });
      }
      return json(route, 200, {
        events_searched: true,
        sessions: [
          {
            id: "B1",
            repo: "shop",
            rules: ["D001", "D003"],
            session: { id: "s1", name: "exfil", agent: "claude-code" },
            created_ms: 0,
          },
        ],
        hits: [
          {
            bundle: "B1",
            session: "exfil",
            repo: "shop",
            seq: 4,
            kind: "effect.file.read_sensitive",
            path: "~/.ssh/id_ed25519",
            target: null,
            detail: null,
          },
        ],
      });
    }
    return json(route, 404, { title: "Not found" });
  });
}

async function seriousViolations(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

test("the Hub dashboard shows sessions, divergence rates, denials, and loss", async ({
  page,
  consoleErrors,
}) => {
  await hub(page);
  await open(page, "/hub");
  await expect(page.getByRole("heading", { level: 1, name: "Hub" })).toBeVisible();
  const rates = page.getByRole("table", { name: "Share of sessions that raised each rule" });
  await expect(rates.getByRole("row", { name: /D001/ })).toContainText("67 %");
  await expect(rates.getByRole("row", { name: /D003/ })).toContainText("33 %");
  const repos = page.getByRole("table", { name: "Uploaded sessions per repository" });
  await expect(repos.getByRole("row", { name: /shop/ })).toContainText("2");
  await expect(page.getByText("Denied actions").first()).toBeVisible();
  await expect(page.getByText("of 120 events · 1 sensor losses")).toBeVisible();
  await expect(page.getByText("not recorded in bundles").first()).toBeVisible();
  // Switching organisations refetches.
  await page.getByLabel("Organisation").selectOption("beta");
  await expect(page).toHaveURL(/org=beta/);
  expect(await seriousViolations(page)).toEqual([]);
  void consoleErrors;
});

test("Hub search lists matching events and points at a malformed query", async ({ page }) => {
  // The 400 answer is logged by the browser as a failed resource; any
  // other console error or warning still fails the test.
  const errors: string[] = [];
  page.on("console", (m) => {
    if ((m.type() === "error" || m.type() === "warning") && !/status of 400/.test(m.text())) {
      errors.push(m.text());
    }
  });
  page.on("pageerror", (e) => errors.push(e.message));
  await hub(page);
  await open(page, "/hub/search");
  const box = page.getByRole("textbox");
  await box.fill("path:~/.ssh rule:D003");
  await page.getByRole("button", { name: "Search" }).click();
  const events = page.getByRole("region", { name: "Matching events" });
  await expect(events.getByRole("row", { name: /exfil/ })).toContainText("~/.ssh/id_ed25519");
  await expect(page.getByRole("region", { name: "Matching sessions" })).toContainText("D001 D003");
  await box.fill("rule:D003 colour:red");
  await page.getByRole("button", { name: "Search" }).click();
  const alert = page.getByRole("alert");
  await expect(alert).toContainText('unknown filter "colour"');
  await expect(alert.locator("mark")).toHaveText("colour:red");
  await expect(box).toHaveAttribute("aria-invalid", "true");
  expect(await seriousViolations(page)).toEqual([]);
  expect(errors).toEqual([]);
});

test("signed out, the Hub pages offer the login", async ({ page }) => {
  await hub(page, false);
  await open(page, "/hub");
  const link = page.getByRole("link", { name: "Sign in to the Hub" });
  await expect(link).toHaveAttribute("href", "/api/v1/auth/oidc/login?return_to=%2Fhub");
});
