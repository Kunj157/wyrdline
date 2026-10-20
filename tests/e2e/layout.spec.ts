import { expect, open, openSession, test } from "./fixtures";

for (const width of [1440, 1024, 768, 390]) {
  test(`the session screen fits ${width} px`, async ({ page, consoleErrors }, info) => {
    await page.setViewportSize({ width, height: 900 });
    await openSession(page);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow, "no horizontal page scroll").toBeLessThanOrEqual(0);
    await expect(
      page.getByRole("region", { name: "Inspector" }).or(page.getByText("Select an event")),
    ).toBeVisible();
    await expect(page.getByRole("table", { name: "Session events in order" })).toBeVisible();
    await page.screenshot({ path: info.outputPath(`session-${width}.png`), fullPage: true });
    void consoleErrors;
  });
}

for (const width of [1440, 1024, 768, 390]) {
  test(`the divergence inbox and session panel fit ${width} px`, async ({
    page,
    consoleErrors,
  }, info) => {
    await page.setViewportSize({ width, height: 900 });
    const overflow = () =>
      page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    await open(page, "/divergences");
    await page.getByRole("button", { name: "All" }).click();
    await expect(page.getByRole("region", { name: /D002 out-of-scope-write/ })).toBeVisible();
    expect(await overflow(), "no horizontal page scroll").toBeLessThanOrEqual(0);
    await page.screenshot({ path: info.outputPath(`inbox-${width}.png`), fullPage: true });

    await openSession(page);
    await page.getByRole("tab", { name: /^Divergences/ }).click();
    const panel = page.getByRole("tabpanel");
    await expect(panel.getByRole("article").first()).toBeVisible();
    // With a note form open, its widest state.
    await panel.getByRole("article").first().getByRole("button").last().click();
    expect(await overflow(), "no horizontal page scroll").toBeLessThanOrEqual(0);
    await panel.screenshot({ path: info.outputPath(`session-divergences-${width}.png`) });
    void consoleErrors;
  });
}
