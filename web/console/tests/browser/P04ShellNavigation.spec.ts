import { expect, test } from "@playwright/test";
import { atlasRoutes, atlasViewports, installAtlasApi, SANDBOX_ID, waitForAtlasRoute } from "./atlas";

async function openAtlas(page: Parameters<typeof installAtlasApi>[0], route: string) {
  await installAtlasApi(page);
  await page.goto(`/atlas.html?route=${encodeURIComponent(route)}`);
}

for (const route of atlasRoutes) {
  for (const viewport of atlasViewports) {
    test(`P04 route frame ${route.name} at ${viewport.name}`, async ({ page }) => {
      await page.setViewportSize(viewport);
      await openAtlas(page, route.path);
      await waitForAtlasRoute(page, route.ready);
      await expect(page.locator("main#main-content")).toHaveCount(1);
      await expect(page.locator("body")).toHaveCSS("overflow-y", "hidden");
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

      if (route.name !== "fleet") {
        await expect(page.locator("[data-route-scroll-owner]")).toHaveCount(1);
      }
    });
  }
}

test("P04 canonicalizes the legacy Layers deep link without losing search or hash", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openAtlas(page, `/sandboxes/${SANDBOX_ID}/layerstack?workspace=fixture#layers`);

  await expect(page.getByText("fixture-layer-2")).toBeVisible();
  await expect(page.locator("[data-atlas-location]")).toHaveText(
    `/sandboxes/${SANDBOX_ID}/observability/layerstack?workspace=fixture#layers`,
  );
  await expect(page.getByRole("tab", { name: "Layers" })).toHaveAttribute("aria-selected", "true");
});

test("P04 redirects the sandbox root to Terminal", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openAtlas(page, `/sandboxes/${SANDBOX_ID}`);

  await expect(page.getByText("No commands yet")).toBeVisible();
  await expect(page.locator("[data-atlas-location]")).toHaveText(
    `/sandboxes/${SANDBOX_ID}/terminal`,
  );
  await expect(page.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
});

test("P04 keeps the cgroup URL while labeling the view Processes", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openAtlas(page, `/sandboxes/${SANDBOX_ID}/observability/cgroup`);
  await waitForAtlasRoute(page, "Workspace process topology");

  await expect(page.locator("[data-atlas-location]")).toHaveText(
    `/sandboxes/${SANDBOX_ID}/observability/cgroup`,
  );
  await expect(page.getByRole("tab", { name: "Processes" })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("[data-console-breadcrumbs]")).toContainText("Processes");
});

test("P04 supports skip focus, scoped tabs, and bounded route scrolling", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openAtlas(page, `/sandboxes/${SANDBOX_ID}/observability/layerstack`);
  await waitForAtlasRoute(page, "fixture-layer-2");

  await page.getByRole("link", { name: "Skip to main content" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("main#main-content")).toBeFocused();

  const sandboxTabs = page.getByRole("tablist", { name: "Sandbox navigation" });
  await sandboxTabs.getByRole("tab", { name: "Observability" }).focus();
  await page.keyboard.press("ArrowLeft");
  await expect(sandboxTabs.getByRole("tab", { name: "Files" })).toBeFocused();
  await expect(page).toHaveScreenshot("p04-shell-keyboard-scroll-1024x768.png", { animations: "disabled" });

  const scrollOwner = page.locator("[data-route-scroll-owner]");
  const scrollMetrics = await scrollOwner.evaluate((element) => {
    const sentinel = document.createElement("div");
    sentinel.setAttribute("aria-hidden", "true");
    sentinel.style.height = "1200px";
    element.append(sentinel);
    element.scrollTop = 120;
    return {
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop,
    };
  });
  expect(scrollMetrics.scrollHeight).toBeGreaterThan(scrollMetrics.clientHeight);
  expect(scrollMetrics.scrollTop).toBeGreaterThan(0);
  expect(await page.evaluate(() => document.scrollingElement?.scrollTop ?? 0)).toBe(0);
});

test("P04 narrow Drawer traps and restores focus", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await openAtlas(page, `/sandboxes/${SANDBOX_ID}/observability/resources`);
  await waitForAtlasRoute(page, "CPU (Δ cpu_usec / s)");

  const trigger = page.getByRole("button", { name: "Open navigation" });
  await trigger.click();
  const drawer = page.getByRole("dialog", { name: "Navigation" });
  await expect(drawer).toBeVisible();
  await expect(drawer.getByRole("link", { name: "↳ Resources" })).toBeFocused();
  await expect(page).toHaveScreenshot("p04-navigation-open-375x812.png", { animations: "disabled" });

  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();
  await expect(trigger).toBeFocused();
});
