import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page, type TestInfo } from "@playwright/test";
import {
  FIXTURE_ROUTE,
  UI_FIXTURE_NAMES,
  type UiFixtureName,
} from "../../fixtures/laboratory";
import { installFixtureApi } from "./api-harness";

const widths = [375, 768, 1024, 1440] as const;

async function selectRun(page: Page, label: "Reference run" | "Candidate run", id: string) {
  await page.getByRole("combobox", { name: label }).click();
  await page.getByRole("option").filter({ hasText: id }).click();
}

async function reachScenario(page: Page, scenario: UiFixtureName): Promise<void> {
  await page.goto(FIXTURE_ROUTE[scenario], { waitUntil: "domcontentloaded" });

  if (scenario === "command-validation-updating") {
    await expect(page.getByText("Updating canonical validation and estimates…", { exact: true })).toBeVisible();
    return;
  }

  if (
    scenario === "command-customize-unchanged" ||
    scenario === "command-customized" ||
    scenario === "command-reset-to-default" ||
    scenario === "command-allowlisted-shell-case" ||
    scenario === "files-publish-warning" ||
    scenario === "workspace-large-insufficient-space" ||
    scenario === "layerstack-n0-control" ||
    scenario === "layerstack-remount-restarts"
  ) {
    await page.getByRole("button", { name: "Customize" }).click();
    await expect(page.getByLabel("Typed experiment configuration")).toBeVisible();
  }

  if (scenario === "command-customized") {
    await page.getByLabel("Seed").fill("20260713");
  }
  if (scenario === "command-reset-to-default") {
    await page.getByRole("button", { name: "Reset all" }).click();
    await expect(page.getByText("Default configuration", { exact: true })).toBeVisible();
  }

  if (scenario.startsWith("compare-")) {
    await selectRun(page, "Reference run", "run-reference");
    await selectRun(page, "Candidate run", "run-candidate");
    await page.getByRole("button", { name: "Check compatibility" }).click();
    await expect(page.getByRole("heading", { name: "Compatibility checks" })).toBeVisible();
  }

  if (scenario.startsWith("run-")) {
    await expect(page.getByRole("heading", { name: "Quick Smoke live evidence" })).toBeVisible();
  } else if (scenario.startsWith("report-")) {
    await expect(page.getByText("Scientific report", { exact: true })).toBeVisible();
  } else if (scenario.startsWith("overview-")) {
    await expect(page.getByRole("heading", { name: "Plan a local, reproducible benchmark" })).toBeVisible();
  } else if (!scenario.startsWith("compare-")) {
    await expect(page.getByText("Research question", { exact: true })).toBeVisible();
  }

  await page.waitForTimeout(100);
}

async function capture(page: Page, testInfo: TestInfo, name: string) {
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ path, fullPage: true, animations: "disabled" });
  await testInfo.attach(name, { path, contentType: "image/png" });
}

for (const scenario of UI_FIXTURE_NAMES) {
  test(`${scenario} remains responsive at all release widths`, async ({ page }, testInfo) => {
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];
    page.on("console", (message) => { if (message.type() === "error") consoleErrors.push(message.text()); });
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await installFixtureApi(page, () => scenario);

    for (const width of widths) {
      await page.setViewportSize({ width, height: 900 });
      await reachScenario(page, scenario);
      expect(await page.locator("html").evaluate((root) => root.scrollWidth <= root.clientWidth + 1)).toBe(true);
      await capture(page, testInfo, `${scenario}-${width}`);
    }

    expect(consoleErrors).toEqual([]);
    expect(pageErrors).toEqual([]);
  });
}

test("LayerStack report renders correlated operation evidence without browser derivation", async ({ page }, testInfo) => {
  const scenario: UiFixtureName = "report-complete-n30";
  await installFixtureApi(page, () => scenario);
  await page.setViewportSize({ width: 375, height: 900 });
  await page.goto("/benchmark/reports/run-reference?view=results", { waitUntil: "domcontentloaded" });

  await page.getByRole("button", { name: /Squash LayerStack cell-layerstack/ }).click();
  await page.getByRole("button", { name: /Squash layerstack.*request-layerstack-0001/i }).click();

  await expect(page.getByText("N — requested live sessions", { exact: true })).toBeVisible();
  await expect(page.getByText("W — effective remount parallelism", { exact: true })).toBeVisible();
  await expect(page.getByText("layer-source-a", { exact: true }).first()).toBeVisible();
  await expect(page.getByText(
    "filesystem_allocation_probe: allocated-byte counter unavailable for this snapshot",
    { exact: true },
  ).first()).toBeVisible();
  for (const snapshot of ["S0", "S1", "S2", "S3"] as const) {
    await expect(page.getByText(snapshot, { exact: true })).toBeVisible();
  }

  expect(await page.locator("html").evaluate((root) => root.scrollWidth <= root.clientWidth + 1)).toBe(true);
  const results = await new AxeBuilder({ page }).exclude(".mantine-VisuallyHidden-root").analyze();
  expect(results.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    targets: violation.nodes.map(({ target }) => target.join(" ")),
  }))).toEqual([]);
  await capture(page, testInfo, "layerstack-operation-evidence-375");
});

test("schema-v4 report renders authored metrics, controls, timelines, checks, and Methods identities", async ({ page }, testInfo) => {
  const scenario: UiFixtureName = "report-complete-n30";
  await installFixtureApi(page, () => scenario);
  await page.setViewportSize({ width: 375, height: 900 });
  await page.goto("/benchmark/reports/run-reference", { waitUntil: "domcontentloaded" });

  await expect(page.getByRole("heading", { name: "Changed & held factors" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Changed across test combinations" }).first()).toBeVisible();
  await expect(page.getByRole("heading", { name: "Held constant" }).first()).toBeVisible();
  await expect(page.getByText("Concurrent requests", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Workspace profile", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Control 1", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Test combinations", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Trial batches", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Issued product requests", { exact: true }).first()).toBeVisible();

  await page.getByRole("tab", { name: "Results & distributions" }).click();
  await expect(page.getByRole("heading", { name: "Factor-study projections" })).toBeVisible();
  await expect(page.getByText(/Trend · Concurrent requests/)).toBeVisible();
  await expect(page.getByText(/Matrix · Live sessions × Remount parallelism/)).toBeVisible();
  await expect(page.getByText(/Raw observations \(60\)/)).toBeVisible();
  await expect(page.getByRole("heading", { name: "Control-value comparisons" }).first()).toBeVisible();
  await expect(page.getByText("exec-command-batch-makespan-control", { exact: true })).toBeVisible();
  await page.getByText("Raw observations (60)", { exact: true }).click();

  const identities = page.getByLabel("Available report metric identities").first();
  for (const id of ["batch_makespan_ns", "request_latency_ns", "throughput_ops_s", "setup_ns", "verify_ns", "teardown_ns"] as const) {
    await expect(identities.getByText(id, { exact: true })).toBeVisible();
  }
  await expect(page.getByText("trial-command-0001", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("963,100", { exact: true }).first()).toBeVisible();

  const metricSelector = page.getByRole("combobox", { name: "Metric selector" }).first();
  await metricSelector.click();
  await page.getByRole("option", { name: "Request latency · nanoseconds" }).click();
  await expect(page.getByText("request-command-0001-01", { exact: true })).toBeVisible();
  await expect(page.getByText("213,800", { exact: true }).first()).toBeVisible();

  await metricSelector.click();
  await page.getByRole("option", { name: "Throughput · operations_per_second" }).click();
  await expect(page.getByText(/ops\/s/).first()).toBeVisible();
  await expect(page.getByText("Not integer-backed", { exact: true }).first()).toBeVisible();

  await page.getByRole("tab", { name: "Resources" }).click();
  await expect(page.getByRole("heading", { name: "Resource timelines" }).first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Resource timeline graphic for trial-measured-0001" })).toBeVisible();
  await expect(page.getByText("Operation window", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Operation start", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Operation duration", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Operation end", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("request-command-0001", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("sample outside the exact request window", { exact: false })).toBeVisible();
  await expect(page.getByText("memory.current disappeared during teardown", { exact: false })).toBeVisible();
  await expect(page.getByText("Total squash", { exact: true }).first()).toBeVisible();

  await page.getByRole("tab", { name: "Correctness" }).click();
  await expect(page.getByRole("heading", { name: "Detailed check evidence" }).first()).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Expected" }).first()).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Actual" }).first()).toBeVisible();
  await expect(page.getByText("checks/command-output.json", { exact: true })).toBeVisible();
  await expect(page.getByText("2 evidence items omitted by the backend bound", { exact: true })).toBeVisible();

  await page.getByRole("tab", { name: "Methods & data" }).click();
  await expect(page.getByRole("heading", { name: "Publication-ready Methods" })).toBeVisible();
  await expect(page.getByText("sandbox-benchmark", { exact: true })).toBeVisible();
  await expect(page.getByText("fixture-small-sha256", { exact: true })).toBeVisible();
  await expect(page.getByText("Operation authorities", { exact: true })).toBeVisible();
  await expect(page.getByText("Derived metric", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("eos_benchmark_observation", { exact: true })).toBeVisible();
  await expect(page.getByText("isolated_loopback_per_execution_block", { exact: true })).toBeVisible();
  await expect(page.getByText("daemon-binary-sha256", { exact: true })).toBeVisible();

  expect(await page.locator("html").evaluate((root) => root.scrollWidth <= root.clientWidth + 1)).toBe(true);
  const results = await new AxeBuilder({ page }).exclude(".mantine-VisuallyHidden-root").analyze();
  expect(results.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    targets: violation.nodes.map(({ target }) => target.join(" ")),
  }))).toEqual([]);
  await capture(page, testInfo, "report-schema-v4-projections-375");
});

test("schema-v4 resource view renders the typed Pearson method and interval", async ({ page }, testInfo) => {
  const scenario: UiFixtureName = "report-cpu-latency-correlation";
  await installFixtureApi(page, () => scenario);
  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto(FIXTURE_ROUTE[scenario], { waitUntil: "domcontentloaded" });

  await expect(page.getByText("pearson", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("eligible trial aggregate by trial id", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("measured product success checks pass cleanup restored", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("batch_makespan_ns", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("sandbox_cpu_time_ns", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("95% Pearson confidence interval", { exact: true })).toBeVisible();
  await expect(page.getByText(/percentile bootstrap pearson.*9,978 valid of 10,000 resamples/)).toBeVisible();
  await expect(page.getByLabel("CPU and latency correlation points")).toBeVisible();

  const results = await new AxeBuilder({ page }).exclude(".mantine-VisuallyHidden-root").analyze();
  expect(results.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    targets: violation.nodes.map(({ target }) => target.join(" ")),
  }))).toEqual([]);
  await capture(page, testInfo, "report-schema-v4-pearson-768");
});

test("live run renders event-authored cell state and synchronized transition evidence", async ({ page }) => {
  const scenario: UiFixtureName = "run-running-setup";
  await installFixtureApi(page, () => scenario);
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.goto("/benchmark/runs/run-fixture", { waitUntil: "domcontentloaded" });

  await expect(page.getByRole("heading", { name: "Cell-state matrix" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Request and phase transition timeline" })).toBeVisible();
  await expect(page.getByRole("row", { name: /cell-command.*Current/ })).toBeVisible();
  await expect(page.getByText("Phase · setup · trial-0004", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Request · request-0004 · trial-0004", { exact: true })).toBeVisible();
});

test("settings edits only the workspace root through the typed runner endpoint", async ({ page }) => {
  const scenario: UiFixtureName = "overview-default-ready";
  await installFixtureApi(page, () => scenario);
  await page.goto("/benchmark", { waitUntil: "domcontentloaded" });
  await page.locator('meta[name="eos-benchmark-nonce"]').evaluate((meta) => {
    meta.setAttribute("content", "fixture-mutation-nonce");
  });

  await page.getByRole("button", { name: "Settings" }).click();
  const input = page.getByRole("textbox", { name: "Test workspace root" });
  await expect(input).toHaveValue("/tmp/eos-benchmark-fixture");
  await input.fill("/tmp/eos-benchmark-next");

  const requestPromise = page.waitForRequest((request) =>
    request.method() === "PUT" && new URL(request.url()).pathname === "/api/v1/settings"
  );
  await page.getByRole("button", { name: "Save workspace root" }).click();
  const request = await requestPromise;

  expect(request.postDataJSON()).toEqual({ test_workspace_root: "/tmp/eos-benchmark-next" });
  expect(await request.headerValue("x-eos-benchmark-nonce")).toBe("fixture-mutation-nonce");
  await expect(page.getByRole("status")).toContainText("Workspace root saved");
  await expect(page.getByText("api_update", { exact: true })).toBeVisible();
});

const primaryRoutes = [
  ["overview", "/benchmark"],
  ["command", "/benchmark/command"],
  ["files", "/benchmark/files"],
  ["workspace", "/benchmark/workspace"],
  ["layerstack", "/benchmark/layerstack"],
  ["run", "/benchmark/runs/run-fixture"],
  ["report", "/benchmark/reports/run-reference"],
  ["compare", "/benchmark/compare"],
] as const;

for (const [name, path] of primaryRoutes) {
  test(`${name} route has no automated accessibility violations`, async ({ page }) => {
    const scenario: UiFixtureName = name === "run"
      ? "run-running-operation"
      : name === "report"
        ? "report-complete-n30"
        : name === "compare"
          ? "compare-compatible"
          : name === "command"
            ? "command-default"
            : name === "files"
              ? "files-publish-warning"
              : name === "workspace"
                ? "workspace-large-insufficient-space"
                : name === "layerstack"
                  ? "layerstack-n0-control"
                  : "overview-default-ready";
    await installFixtureApi(page, () => scenario);
    await page.setViewportSize({ width: 1024, height: 900 });
    await page.goto(path, { waitUntil: "domcontentloaded" });
    await expect(page.locator("#main-content")).toBeVisible();
    const results = await new AxeBuilder({ page }).exclude(".mantine-VisuallyHidden-root").analyze();
    expect(results.violations.map((violation) => ({
      id: violation.id,
      impact: violation.impact,
      targets: violation.nodes.map(({ target }) => target.join(" ")),
    }))).toEqual([]);
  });
}
