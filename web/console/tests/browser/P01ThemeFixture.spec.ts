import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const STANDARD_VIEWPORTS = [
  [375, 812],
  [768, 1024],
  [1024, 768],
  [1440, 900],
] as const;

async function openFixture(page: Page) {
  await page.goto("/p01-theme.html");
  await expect(page.getByRole("heading", { name: "EphemeralOS operator theme" })).toBeVisible();
}

for (const [width, height] of STANDARD_VIEWPORTS) {
  test(`P01 theme specimen at ${width}x${height} @visual`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await openFixture(page);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    await expect(page).toHaveScreenshot(`p01-theme-${width}x${height}.png`, {
      animations: "disabled",
      fullPage: true,
    });
  });
}

for (const [width, height] of [
  [375, 812],
  [1440, 900],
] as const) {
  test(`P01 state specimen at ${width}x${height} @visual`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await openFixture(page);
    await page.getByRole("button", { name: "Primary action" }).focus();
    await page.getByRole("button", { name: "Destructive" }).hover();
    await expect(page.getByRole("button", { name: "Primary action" })).toBeFocused();
    await expect(page).toHaveScreenshot(`p01-theme-states-${width}x${height}.png`, {
      animations: "disabled",
      fullPage: true,
    });
  });

  test(`P01 reduced-motion and logo specimen at ${width}x${height} @visual`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await openFixture(page);
    await expect(page.getByTestId("motion-state")).toHaveText("motion: reduced");
    const logo = page.getByTestId("brand-logo");
    await expect(logo).toHaveAttribute("alt", "");
    await expect(logo).toHaveJSProperty("naturalWidth", 1024);
    await expect(logo).toHaveJSProperty("naturalHeight", 1024);
    await expect(logo).toHaveCSS("object-fit", "contain");
    await expect(page).toHaveScreenshot(`p01-theme-reduced-${width}x${height}.png`, {
      animations: "disabled",
      fullPage: true,
    });
  });
}

test("P01 theme specimen has no Axe violations @a11y", async ({ page }) => {
  await openFixture(page);
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
});
