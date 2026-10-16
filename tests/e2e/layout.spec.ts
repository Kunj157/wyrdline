import { expect, openSession, test } from "./fixtures";

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
