import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import type { WorkspaceProcessTopology } from "../../src/api/observability";
import { measureFromTimestampToPaintP95, measureInputToPaintP95 } from "./performance";

const NOW = 1_700_004_000_000;

type FixtureApi = {
  eventCalls: () => number;
  failEvents: () => void;
  failCgroup: () => void;
  setTopology: (value: WorkspaceProcessTopology) => void;
};

function makeEvents(count = 2_000) {
  return Array.from({ length: count }, (_, index) => ({
  ts: NOW - (count - 1 - index) * 1_000,
  trace: "trace-2k",
  parent: index % 2 ? "span-root" : null,
  name: `event-${String(index).padStart(4, "0")}`,
  attrs: { index, source: "P07 fixture", detail: `event detail ${index}` },
  }));
}

const events = makeEvents();

const samples = Array.from({ length: 16 }, (_, index) => ({
  ts: NOW - (15 - index) * 5_000,
  sample_delta_ms: 5_000,
  metrics: { cgroup_available: true, disk_bytes: 4_000_000 + index * 25_000, mem_cur: 24_000_000 + index * 90_000 },
  deltas: { cpu_usec: 10_000 + index * 500, io_rbytes: 500 + index * 10, io_wbytes: 200 + index * 5 },
}));

const topology: WorkspaceProcessTopology = {
  schema_version: 2,
  available: true,
  source: "proc_namespaces",
  error: null,
  truncated: false,
  warnings: [],
  workspaces: [
    {
      workspace_id: "workspace-active",
      state: "active",
      holder_pid: 101,
      pid_namespace: "pid:[1001]",
      mount_namespace: "mnt:[2001]",
      processes: [
        { pid: 201, namespace_pid: 1, parent_pid: 101, name: "ns-init", state: "S (sleeping)", kind: "namespace_init", cgroup_memberships: ["0::/"] },
        { pid: 233, namespace_pid: 2, parent_pid: 201, name: "bash", state: "S (sleeping)", kind: "process", cgroup_memberships: ["0::/", "2:cpu:/fixture"] },
      ],
    },
    {
      workspace_id: "workspace-idle",
      state: "idle",
      holder_pid: 102,
      pid_namespace: "pid:[1002]",
      mount_namespace: "mnt:[2002]",
      processes: [
        { pid: 301, namespace_pid: 1, parent_pid: 102, name: "ns-init", state: "S (sleeping)", kind: "namespace_init", cgroup_memberships: [] },
      ],
    },
    {
      workspace_id: "workspace-partial",
      state: "partial",
      holder_pid: 103,
      pid_namespace: null,
      mount_namespace: null,
      processes: [],
    },
  ],
};

function traceFor(fixtureEvents: typeof events) {
  return {
  view: "trace",
  trace: "trace-2k",
  spans: [{
    offset_ms: 0,
    span: { ts: NOW - 2_000, trace: "trace-2k", span: "span-root", name: "fixture.root", dur_ms: 2_000, status: "completed", attrs: { fixture: true } },
    events: [{ offset_ms: 500, event: fixtureEvents.at(-1) }],
    children: Array.from({ length: 1_999 }, (_, index) => ({
      offset_ms: (index + 1) % 1_800,
      span: {
        ts: NOW - 1_900 + index,
        trace: "trace-2k",
        span: `span-${String(index + 1).padStart(4, "0")}`,
        name: `fixture.child.${index + 1}`,
        dur_ms: 10 + (index % 40),
        status: index % 11 === 0 ? "error" : "completed",
        attrs: index === 1_500 ? { target: "mid-trace detail" } : {},
      },
      events: [],
      children: [],
    })),
  }],
  };
}

const trace = traceFor(events);

async function installObservabilityApi(
  page: Page,
  eventCount = 2_000,
  includeTopology = false,
): Promise<FixtureApi> {
  const fixtureEvents = eventCount === 2_000 ? events : makeEvents(eventCount);
  const fixtureTrace = eventCount === 2_000 ? trace : traceFor(fixtureEvents);
  let eventsFail = false;
  let eventsCalls = 0;
  let cgroupFail = false;
  let currentTopology = topology;
  await page.route("**/api/rpc", async (route) => {
    const { op, args } = route.request().postDataJSON() as { op: string; args: Record<string, unknown> };
    if (op === "events") {
      eventsCalls += 1;
      if (eventsFail) {
        await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: { kind: "fixture_error", message: "event stream unavailable" } }) });
        return;
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify({ view: "events", events: fixtureEvents }) });
      return;
    }
    if (op === "trace") {
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(fixtureTrace) });
      return;
    }
    if (op === "cgroup") {
      if (cgroupFail) {
        await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: { kind: "fixture_error", message: "topology refresh unavailable" } }) });
        return;
      }
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          view: "cgroup",
          scope: String(args.scope ?? "sandbox"),
          series: samples,
          ...(includeTopology ? { topology: currentTopology } : {}),
        }),
      });
      return;
    }
    if (op === "layerstack") {
      const body = args.layer_id
        ? {
            view: "layerstack",
            layer_id: args.layer_id,
            entries: Array.from({ length: 500 }, (_, index) => ({ path: `src/fixture-${index}.ts`, kind: index % 7 === 0 ? "delete" : "file" })),
            truncated: true,
          }
        : args.workspace_id
          ? { view: "layerstack", workspace: args.workspace_id, mounts: [{ layer_id: "fixture-layer-2", shared_with: ["workspace-fixture"] }], upper_bytes: 12_000 }
          : {
              view: "layerstack",
              manifest_version: 7,
              root_hash: "fixture-root-hash",
              active_lease_count: 1,
              total_bytes: 6_000_000,
              layers: [
                { layer_id: "fixture-layer-2", bytes: 3_000_000, leased_by_workspaces: 0, booked_by: [] },
                { layer_id: "fixture-layer-1", bytes: 2_000_000, leased_by_workspaces: 1, booked_by: ["workspace-fixture"] },
                { layer_id: "fixture-base", bytes: 1_000_000, leased_by_workspaces: 0, booked_by: [] },
              ],
            };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
      return;
    }
    await route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: { kind: "fixture_error", message: `unexpected operation ${op}` } }) });
  });
  return {
    eventCalls: () => eventsCalls,
    failEvents: () => { eventsFail = true; },
    failCgroup: () => { cgroupFail = true; },
    setTopology: (value) => { currentTopology = value; },
  };
}

async function openView(
  page: Page,
  view: "events" | "resources" | "cgroup" | "traces" | "layers",
  eventCount = 2_000,
  includeTopology = false,
) {
  const api = await installObservabilityApi(page, eventCount, includeTopology || view === "cgroup");
  await page.clock.setFixedTime(NOW);
  await page.goto(`/p07-observability.html?view=${view}`);
  const ready = {
    events: `event-${String(eventCount - 1).padStart(4, "0")}`,
    resources: "CPU (Δ cpu_usec / s)",
    cgroup: "Workspace process topology",
    traces: "fixture.root",
    layers: "fixture-layer-2",
  }[view];
  await expect(page.getByText(ready, { exact: false })).toBeVisible();
  return api;
}

for (const [width, height] of [
  [375, 812],
  [768, 1024],
  [1024, 768],
  [1440, 900],
] as const) {
  for (const view of ["events", "resources", "cgroup", "traces", "layers"] as const) {
    test(`P07 ${view} Mantine surface at ${width}x${height} @visual`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      await openView(page, view);
      await expect(page).toHaveScreenshot(`p07-${view}-${width}x${height}.png`, { animations: "disabled" });
    });
  }
}

test("P07 Events keeps selection and expanded attrs while polling, pauses on demand, and exposes the last confirmed error", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const api = await openView(page, "events");
  const tableRows = page.locator("[data-event-row]");
  await expect.poll(() => tableRows.count()).toBeLessThan(128);
  const selected = tableRows.first();
  await selected.focus();
  await page.keyboard.press("Space");
  await expect(selected).toHaveAttribute("aria-selected", "true");
  await selected.getByTitle("Toggle full attributes").click();
  await expect(selected.getByRole("button")).toHaveAttribute("aria-expanded", "true");

  await page.getByRole("button", { name: "tail", exact: true }).click();
  await expect(page.locator("[data-event-tail-state]")).toHaveAttribute("data-event-tail-state", "paused");
  await page.waitForTimeout(100);
  const callsAfterPause = api.eventCalls();
  await page.waitForTimeout(700);
  expect(api.eventCalls()).toBe(callsAfterPause);

  api.failEvents();
  await page.getByRole("button", { name: "resume tail" }).click();
  await expect(page.getByText("Refresh paused on last confirmed events")).toBeVisible();
  await expect(selected).toHaveAttribute("aria-selected", "true");
  await expect(page).toHaveScreenshot("p07-events-paused-stale-expanded-1440x900.png", { animations: "disabled" });
  await page.setViewportSize({ width: 375, height: 812 });
  await expect(page).toHaveScreenshot("p07-events-paused-stale-expanded-375x812.png", { animations: "disabled" });
});

test("P07 Events sort through the TanStack table model without unbounded DOM rows", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "events");
  await page.getByRole("button", { name: "name" }).click();
  await expect(page.locator("[data-event-row]").first()).toContainText("event-0000");
  await expect.poll(() => page.locator("[data-event-row]").count()).toBeLessThan(128);
});

test("P12 keeps 10k Events sorting below the input-to-paint budget", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "events", 10_000);
  const rows = page.locator("[data-event-row]");
  const sort = page.getByRole("button", { name: "name" });
  await expect.poll(() => rows.count()).toBeLessThan(128);
  await page.getByRole("button", { name: "tail", exact: true }).click();
  await expect(page.getByRole("button", { name: "resume tail", exact: true })).toBeVisible();
  await sort.click();
  await sort.click();
  await sort.click();

  await measureInputToPaintP95(page, "Events Table 10k sort", async () => {
    await sort.click();
    await expect(rows.first()).toBeVisible();
  });
});

test("P07 virtualizes a 2K-span waterfall and preserves an independent overflow owner", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "traces");
  const waterfall = page.locator("[data-trace-waterfall]");
  await expect.poll(() => waterfall.locator("[data-trace-span]").count()).toBeLessThan(128);
  await waterfall.evaluate(async (element) => {
    element.scrollTop = 34 * 1_500;
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  });
  await expect(page.locator('[data-trace-span="span-1500"]')).toBeVisible();
  await expect(page).toHaveScreenshot("p07-traces-2k-mid-1440x900.png", { animations: "disabled" });
});

test("P12 keeps 2k Trace waterfall scrolling below the input-to-paint budget", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "traces");
  const waterfall = page.locator("[data-trace-waterfall]");
  let lastIndex = 0;

  await measureFromTimestampToPaintP95(page, "Trace 2k waterfall scroll", async (iteration) => {
    lastIndex = (iteration * 97) % 1_999;
    return waterfall.evaluate((element, target) => {
      const startedAt = performance.now();
      element.scrollTop = target / 1_999 * (element.scrollHeight - element.clientHeight);
      return startedAt;
    }, lastIndex);
  });
  await expect(page.locator(`[data-trace-span="span-${String(lastIndex + 1).padStart(4, "0")}"]`)).toBeVisible();
});

test("P07 exposes selected span detail without expanding the 2K waterfall", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "traces");
  const root = page.locator('[data-trace-span="span-root"]');
  await root.getByTitle("fixture.root attributes").click();
  await expect(root).toContainText("fixture");
  await expect(root.getByRole("button")).toHaveAttribute("aria-expanded", "true");
  await expect(page).toHaveScreenshot("p07-traces-span-detail-1440x900.png", { animations: "disabled" });
});

test("P07 provides a narrow trace drawer while the waterfall keeps its own horizontal scroll", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await openView(page, "traces");
  await page.getByRole("button", { name: "Choose trace" }).click();
  await expect(page.getByRole("dialog", { name: "Trace selector" })).toBeVisible();
  await expect(page).toHaveScreenshot("p07-traces-drawer-375x812.png", { animations: "disabled" });
  await page.getByRole("dialog").getByText("last trace", { exact: true }).click();
  const geometry = await page.locator("[data-trace-waterfall]").evaluate((element) => ({ clientWidth: element.clientWidth, scrollWidth: element.scrollWidth }));
  expect(geometry.scrollWidth).toBeGreaterThan(geometry.clientWidth);
  await expect(page).toHaveScreenshot("p07-traces-drawer-overflow-375x812.png", { animations: "disabled" });
});

test("P07 resource charts retain accessible summaries through a resize", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openView(page, "resources");
  await expect(page.locator("[aria-label$='numerical summary']")).toHaveCount(4);
  await page.setViewportSize({ width: 1440, height: 900 });
  await expect(page.locator("[data-resources-view] canvas")).toHaveCount(4);
});

test("P07 cgroup view shows process placement reported by the daemon", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await openView(page, "cgroup");
  await expect(page.locator("[data-process-topology]")).toContainText("Workspace process topology");
  await expect(page.locator('[data-workspace-id="workspace-active"]')).toContainText("pid:[1001]");
  const worker = page.locator('[data-workspace-id="workspace-active"] [data-process-pid="233"]:visible');
  await expect(worker).toContainText("bash");
  await expect(worker).toContainText("2:cpu:/fixture");
  await expect(page.locator('[data-workspace-id="workspace-idle"] [data-workspace-idle]')).toBeVisible();
  await expect(page.locator('[data-workspace-id="workspace-partial"]')).toContainText("Workspace topology is partial");
  await expect(page.locator("[data-process-topology]")).not.toContainText("delegated");
});

test("P07 process topology distinguishes empty, unavailable, and stale refresh states", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const api = await openView(page, "cgroup");

  api.setTopology({ ...topology, workspaces: [] });
  await page.reload();
  await expect(page.locator("[data-process-topology-empty]")).toContainText("No active workspaces");

  api.setTopology({ ...topology, available: false, source: null, error: "procfs enumeration failed", workspaces: [] });
  await page.reload();
  await expect(page.getByRole("alert").filter({ hasText: "procfs enumeration failed" })).toBeVisible();

  api.setTopology(topology);
  await page.reload();
  await expect(page.locator('[data-workspace-id="workspace-active"]')).toBeVisible();
  api.failCgroup();
  await expect(page.getByRole("alert").filter({ hasText: "topology refresh unavailable" })).toBeVisible({ timeout: 4_000 });
  await expect(page.locator('[data-workspace-id="workspace-active"]')).toBeVisible();
});

test("P07 process fields stack without document overflow on narrow screens", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await openView(page, "cgroup");
  await expect(page.locator('[data-workspace-id="workspace-active"] [data-process-pid="233"]:visible')).toContainText("Namespace PID");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("P12 keeps uPlot chart resize repainting below the input-to-paint budget", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "resources");
  const charts = page.locator("[data-resources-view] canvas");
  await expect(charts).toHaveCount(4);

  await measureInputToPaintP95(page, "uPlot chart resize", async (iteration) => {
    await page.setViewportSize({ width: iteration % 2 ? 1439 : 1440, height: 900 });
    await expect(charts).toHaveCount(4);
  });
});

test("P07 layers makes the backend's first-500 limit explicit", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openView(page, "layers");
  await expect(page.getByText("Showing the first 500 entries returned by the backend.")).toBeVisible();
  await page.locator('[data-layer-row="fixture-layer-1"]').click();
  await expect(page.locator('[data-layer-row="fixture-layer-1"]')).toHaveAttribute("aria-pressed", "true");
});

for (const view of ["events", "resources", "cgroup", "traces", "layers"] as const) {
  test(`P07 ${view} has no Axe violations @a11y`, async ({ page }) => {
    await openView(page, view);
    expect((await new AxeBuilder({ page }).disableRules("page-has-heading-one").analyze()).violations).toEqual([]);
  });
}
