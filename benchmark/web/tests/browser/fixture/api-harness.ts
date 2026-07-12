import type { Page, Route } from "@playwright/test";
import type { ExperimentPlan } from "@/api/types";
import {
  DEFINITIONS_FIXTURE,
  comparisonFixture,
  eventFixtures,
  healthFixture,
  reportFixture,
  runFixture,
  runsFixture,
  settingsFixture,
  validationFixture,
  type UiFixtureName,
} from "../../fixtures/laboratory";

function json(route: Route, body: unknown, status = 200) {
  return route.fulfill({
    status,
    contentType: "application/json; charset=utf-8",
    headers: { "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" },
    body: JSON.stringify(body),
  });
}

export async function installFixtureApi(
  page: Page,
  currentScenario: () => UiFixtureName,
): Promise<void> {
  let updatedWorkspaceRoot: string | null = null;
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const scenario = currentScenario();

    if (path === "/api/v1/health") return json(route, healthFixture(scenario));
    if (path === "/api/v1/settings") {
      const settings = settingsFixture(scenario);
      if (request.method() === "PUT") {
        const payload = request.postDataJSON() as { test_workspace_root: string };
        updatedWorkspaceRoot = payload.test_workspace_root;
        return json(route, {
          ...settings,
          test_workspace_root: updatedWorkspaceRoot,
          source: "api_update",
        });
      }
      return json(route, updatedWorkspaceRoot === null ? settings : {
        ...settings,
        test_workspace_root: updatedWorkspaceRoot,
        source: "api_update",
      });
    }
    if (path === "/api/v1/definitions") return json(route, DEFINITIONS_FIXTURE);
    if (path === "/api/v1/plans/validate") {
      if (scenario === "command-validation-updating") await new Promise((resolve) => setTimeout(resolve, 1_500));
      const payload = request.postDataJSON() as { plan: ExperimentPlan };
      return json(route, validationFixture(payload.plan, scenario));
    }
    if (path === "/api/v1/runs") return json(route, runsFixture());
    if (path === "/api/v1/compare") {
      const payload = request.postDataJSON() as { descriptive_override?: boolean };
      return json(route, comparisonFixture(scenario, payload.descriptive_override === true));
    }
    if (/^\/api\/v1\/runs\/[^/]+\/report$/.test(path)) return json(route, reportFixture(scenario));
    if (/^\/api\/v1\/runs\/[^/]+\/artifacts$/.test(path)) {
      return json(route, { schema_version: 1, run_id: "run-reference", artifacts: [] });
    }
    if (/^\/api\/v1\/runs\/[^/]+\/events$/.test(path)) {
      const events = eventFixtures(scenario);
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream; charset=utf-8",
        headers: { "Cache-Control": "no-cache, no-store" },
        body: events.map((event) =>
          `id: ${event.sequence}\nevent: ${event.data.kind}\ndata: ${JSON.stringify(event)}\n\n`
        ).join(""),
      });
    }
    if (/^\/api\/v1\/runs\/[^/]+\/cancel$/.test(path)) {
      return json(route, { schema_version: 1, run_id: "run-fixture", state: "cancelling", cancellation_requested: true }, 202);
    }
    if (/^\/api\/v1\/runs\/[^/]+$/.test(path)) return json(route, runFixture(scenario));

    return json(route, {
      error: {
        code: "fixture_endpoint_missing",
        message: `No fixture exists for ${request.method()} ${path}.`,
        details: null,
        request_id: "fixture-request",
      },
    }, 404);
  });
}
