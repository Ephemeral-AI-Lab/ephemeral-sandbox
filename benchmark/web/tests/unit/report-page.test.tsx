import { MantineProvider } from "@mantine/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { benchmarkApi } from "@/api/client";
import { ReportPage } from "@/pages/ReportPage";
import { benchmarkTheme } from "@/theme";
import { reportFixture } from "../fixtures/laboratory";

vi.mock("@/components/DistributionEvidence", () => ({
  DistributionEvidence: () => <div data-testid="distribution-evidence" />,
}));

function renderReportPage() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <MantineProvider theme={benchmarkTheme}>
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/benchmark/reports/run-reference?view=results"]}>
          <Routes>
            <Route path="/benchmark/reports/:runId" element={<ReportPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>
    </MantineProvider>,
  );
}

afterEach(() => vi.restoreAllMocks());

describe("report metric selector", () => {
  it("accepts report-authored metrics with the same definition id from distinct sources", async () => {
    const report = reportFixture("report-complete-n30");
    const cell = report.cells[0]!;
    const duplicateMetric = structuredClone(cell.metrics[0]!);
    duplicateMetric.identity = {
      ...duplicateMetric.identity,
      source: "independent_resource_observation",
    };
    cell.metrics = [cell.metrics[0]!, duplicateMetric];
    vi.spyOn(benchmarkApi, "report").mockResolvedValue(report);

    renderReportPage();

    const selector = await screen.findByRole("combobox", { name: "Metric selector" });
    expect(selector).toBeTruthy();
    const duplicateRows = screen
      .getAllByText("batch_makespan_ns")
      .map((element) => element.closest("tr"))
      .filter((row): row is HTMLTableRowElement => row !== null);
    expect(duplicateRows).toHaveLength(2);
    expect(document.querySelectorAll(".metric-selector-selected-row")).toHaveLength(1);
  });
});
