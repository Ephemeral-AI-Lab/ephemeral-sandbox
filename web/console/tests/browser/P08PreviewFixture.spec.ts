import { expect, test, type Page } from "@playwright/test";
import { installAtlasApi, SANDBOX_ID } from "./atlas";

const previewRoute = `/sandboxes/${SANDBOX_ID}/preview?port=5173`;
const previewPrefix = `/s/${SANDBOX_ID}/shared/5173`;
const iframeTitle = `preview of ${SANDBOX_ID} port 5173`;

async function openPreview(page: Page, body: string, status = 200) {
  await page.route(`**${previewPrefix}/**`, (route) => route.fulfill({ status, contentType: "text/html", body }));
  await installAtlasApi(page);
  await page.goto(`/atlas.html?route=${encodeURIComponent(previewRoute)}`);
  return page.frameLocator(`iframe[title="${iframeTitle}"]`);
}

for (const [width, height] of [
  [375, 812],
  [768, 1024],
  [1024, 768],
  [1440, 900],
] as const) {
  test(`P08 Preview opaque normal state at ${width}x${height} @visual`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    const preview = await openPreview(page, "<main><h1>Preview ready</h1><p>Untrusted app output remains inside the opaque frame.</p></main>");
    await expect(preview.getByRole("heading", { name: "Preview ready" })).toBeVisible();
    await expect(page).toHaveScreenshot(`p08-preview-success-${width}x${height}.png`, { animations: "disabled" });
  });
}

test("P08 Preview exposes its opaque loading state @visual", async ({ page }) => {
  let release: (() => void) | undefined;
  await page.route(`**${previewPrefix}/**`, async (route) => {
    await new Promise<void>((resolve) => { release = resolve; });
    await route.fulfill({ contentType: "text/html", body: "<h1>Preview ready</h1>" });
  });
  await installAtlasApi(page);
  const navigation = page.goto(`/atlas.html?route=${encodeURIComponent(previewRoute)}`);
  await expect(page.getByRole("status")).toHaveText("Loading preview…");
  await expect(page).toHaveScreenshot("p08-preview-loading-1440x900.png", { animations: "disabled" });
  release?.();
  await navigation;
});

test("P08 Preview keeps proxy failures inside the opaque frame @visual", async ({ page }) => {
  const preview = await openPreview(page, "<main><h1>Preview unavailable</h1><p>The selected Preview route could not be reached.</p></main>", 502);
  await expect(preview.getByRole("heading", { name: "Preview unavailable" })).toBeVisible();
  await expect(page).toHaveScreenshot("p08-preview-error-1440x900.png", { animations: "disabled" });
});

test("P08 Preview keeps an opaque frame boundary while preserving selected capabilities", async ({ page }) => {
  let opaqueApiOrigin: string | undefined;
  await page.route(`**${previewPrefix}/**`, async (route) => {
    const path = new URL(route.request().url()).pathname;
    await route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><body>
        <output id=script>pending</output>
        <output id=parent>pending</output>
        <output id=popup>pending</output>
        <output id=top>pending</output>
        <output id=api>pending</output>
        <script>
          document.querySelector('#script').textContent = 'script ran';
          try { parent.document.body.dataset.previewEscaped = 'yes'; } catch { document.querySelector('#parent').textContent = 'blocked'; }
          document.querySelector('#popup').textContent = window.open('about:blank') === null ? 'blocked' : 'opened';
          try { top.location.href = '/api/rpc'; } catch { document.querySelector('#top').textContent = 'blocked'; }
          fetch('/api/rpc', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' })
            .then((response) => { document.querySelector('#api').textContent = response.status === 403 ? 'guarded' : 'unexpected'; })
            .catch(() => { document.querySelector('#api').textContent = 'blocked'; });
        </script>
      </body>`,
    });
  });
  await installAtlasApi(page);
  await page.route("**/api/rpc", async (route) => {
    opaqueApiOrigin = route.request().headers().origin;
    await route.fulfill({ status: 403, body: "opaque origins cannot call console APIs" });
  });
  await page.goto(`/atlas.html?route=${encodeURIComponent(previewRoute)}`);

  const iframe = page.getByTitle(iframeTitle);
  const preview = page.frameLocator(`iframe[title="${iframeTitle}"]`);
  await expect(iframe).toHaveAttribute("sandbox", "allow-scripts");
  await expect(iframe).toHaveAttribute("allow", "");
  await expect(iframe).toHaveAttribute("referrerpolicy", "no-referrer");
  await expect(preview.locator("#script")).toHaveText("script ran");
  await expect(preview.locator("#parent")).toHaveText("blocked");
  await expect(preview.locator("#popup")).toHaveText("blocked");
  await expect(preview.locator("#top")).toHaveText("blocked");
  await expect(preview.locator("#api")).toHaveText("guarded");
  expect(opaqueApiOrigin).toBe("null");
  await expect(page.locator("[data-atlas-location]")).toHaveText(previewRoute);
  expect(await iframe.evaluate((frame) => {
    try {
      void frame.contentWindow?.location.href;
      return false;
    } catch {
      return true;
    }
  })).toBe(true);

});
