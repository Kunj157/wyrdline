import { expect, openSession, test } from "./fixtures";

test("the river draws with the best renderer and selects on click", async ({
  page,
  consoleErrors,
}, info) => {
  await openSession(page);
  const app = page.locator("[data-renderer]");
  const expected = info.project.name === "webgpu" ? "webgpu" : "canvas2d";
  await expect(app).toHaveAttribute("data-renderer", expected);
  const box = await page.locator("canvas").boundingBox();
  if (!box) throw new Error("no canvas");
  // The fixture's first knot is checkpoint c1 on the spine (world x 14, y 96).
  await page.mouse.click(box.x + 14, box.y + 96);
  await expect(page.getByRole("region", { name: "Inspector" })).toContainText("checkpoint c1");
  await expect(page.getByRole("row", { selected: true })).toHaveCount(1);
  // Zoom keys keep working.
  await app.focus();
  await page.keyboard.press("]");
  await page.keyboard.press("[");
  void consoleErrors;
});

test("without any canvas the SVG river and the table remain", async ({ page, consoleErrors }) => {
  await page.addInitScript(() => {
    HTMLCanvasElement.prototype.getContext = () => null;
  });
  await openSession(page);
  await expect(page.locator("svg[aria-roledescription='causal river']")).toBeVisible();
  await expect(page.getByRole("table", { name: "Session events in order" })).toBeVisible();
  void consoleErrors;
});
