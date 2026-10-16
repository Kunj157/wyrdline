import { test as base, expect, type Page } from "@playwright/test";

/** Browser messages that are not ours: Chrome logs this when WebGPU feature detection finds no adapter. */
const BROWSER_NOISE = [/^No available adapters\.$/];

export const test = base.extend<{ consoleErrors: string[] }>({
  consoleErrors: async ({ page }, use) => {
    const errors: string[] = [];
    page.on("console", (m) => {
      if (
        (m.type() === "error" || m.type() === "warning") &&
        !BROWSER_NOISE.some((r) => r.test(m.text()))
      ) {
        errors.push(`${m.type()}: ${m.text()}`);
      }
    });
    page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
    await use(errors);
    expect(errors, "console errors and warnings").toEqual([]);
  },
});

export { expect };

export const env = () => ({
  base: process.env.WYRD_E2E_BASE ?? "",
  token: process.env.WYRD_E2E_TOKEN ?? "",
  sid: process.env.WYRD_E2E_SID ?? "",
});

/** Open Studio the way `wyrd studio` does (token in the fragment). */
export async function open(page: Page, path = "/sessions") {
  const { base, token } = env();
  await page.goto(`${base}${path}#token=${token}`);
}

export async function openSession(page: Page) {
  await open(page);
  await page.getByRole("link", { name: "e2e greeting fix" }).click();
  await expect(page.getByRole("application", { name: /Causal river/ })).toBeVisible();
  await page.waitForFunction(
    () => document.querySelector("[data-renderer]")?.getAttribute("data-renderer") !== "starting",
  );
}
