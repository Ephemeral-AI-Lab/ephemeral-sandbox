import { expect, test, type Page } from "@playwright/test";

const VIEWPORTS = [
  [375, 812],
  [1440, 900],
] as const;

async function openFixture(page: Page) {
  await page.goto("/p03-primitives.html");
  await expect(page.getByRole("heading", { name: "Console controls and operator states" })).toBeVisible();
}

for (const [width, height] of VIEWPORTS) {
  test("P09 Mantine interaction parity at " + width + "x" + height + " @visual", async ({ page }) => {
    await page.setViewportSize({ width, height });
    await openFixture(page);

    await page.getByRole("tab", { name: "Events" }).click();
    await expect(page.getByText("No new events.")).toBeVisible();
    const imageSelect = page.getByRole("combobox", { name: "Base image" });
    await imageSelect.click();
    await page.getByRole("option", { name: "python:3.13-slim" }).click();
    await expect(imageSelect).toHaveValue("python:3.13-slim");
    await page.getByRole("button", { name: /Workspace target/ }).click();
    await page.getByRole("option", { name: "Session a7f3" }).click();
    await expect(page.getByRole("button", { name: /Workspace target/ })).toContainText("Session a7f3");
    await expect(page).toHaveScreenshot("p09-selection-" + width + "x" + height + ".png", {
      animations: "disabled",
      fullPage: true,
    });

    await page.getByRole("button", { name: "Show notification stack" }).click();
    await expect(page.getByText("policy_denied")).toBeVisible();
    await page.getByRole("button", { name: "More actions" }).click();
    await expect(page.getByRole("menuitem", { name: "Open files" })).toBeVisible();
    await expect(page).toHaveScreenshot("p09-menu-" + width + "x" + height + ".png", {
      animations: "disabled",
      fullPage: true,
    });
    await page.keyboard.press("Escape");

    await page.getByRole("button", { name: "Open popover" }).click();
    await expect(page.getByText("Published workspace selected.")).toBeVisible();
    await page.getByRole("button", { name: "Command metadata" }).hover();
    await expect(page.getByRole("tooltip")).toHaveText("Command metadata");
    await expect(page).toHaveScreenshot("p09-overlays-" + width + "x" + height + ".png", {
      animations: "disabled",
      fullPage: true,
    });
    await page.getByRole("button", { name: "Open popover" }).click();

    const modalTrigger = page.getByRole("button", { name: "Open modal" });
    await modalTrigger.focus();
    await modalTrigger.click();
    const modal = page.getByRole("dialog", { name: "Destroy sandbox" });
    await expect(modal).toBeVisible();
    await expect(page.getByLabel("Type sandbox id to confirm")).toBeFocused();
    await expect(page).toHaveScreenshot("p09-modal-" + width + "x" + height + ".png", {
      animations: "disabled",
      fullPage: true,
    });
    await page.keyboard.press("Escape");
    await expect(modal).toBeHidden();
    await expect(modalTrigger).toBeFocused();

    const drawerTrigger = page.getByRole("button", { name: "Open drawer" });
    await drawerTrigger.focus();
    await drawerTrigger.click();
    const drawer = page.getByRole("dialog", { name: "Workspace details" });
    await expect(drawer).toBeVisible();
    await expect(page.getByLabel("Filter workspace files")).toBeFocused();
    await expect(page).toHaveScreenshot("p09-drawer-" + width + "x" + height + ".png", {
      animations: "disabled",
      fullPage: true,
    });
    await page.keyboard.press("Escape");
    await expect(drawer).toBeHidden();
    await expect(drawerTrigger).toBeFocused();
  });
}
