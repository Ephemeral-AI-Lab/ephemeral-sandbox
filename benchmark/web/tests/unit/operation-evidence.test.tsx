import { MantineProvider } from "@mantine/core";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { OperationEvidence } from "@/components/OperationEvidence";
import { benchmarkTheme } from "@/theme";
import { LAYERSTACK_OPERATION_EVIDENCE_FIXTURE } from "../fixtures/laboratory";

describe("operation evidence", () => {
  it("renders LayerStack request, disposition, allocation, and S0–S3 evidence without deriving values", async () => {
    const user = userEvent.setup();
    render(
      <MantineProvider theme={benchmarkTheme}>
        <OperationEvidence evidence={[LAYERSTACK_OPERATION_EVIDENCE_FIXTURE]} />
      </MantineProvider>,
    );

    expect(screen.getByText("request-layerstack-0001")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /squash layerstack/i }));

    expect(screen.getByText("N — requested live sessions")).toBeTruthy();
    expect(screen.getByText("M — observed migrated")).toBeTruthy();
    expect(screen.getByText("I — observed non-migrated")).toBeTruthy();
    expect(screen.getByText("W — effective remount parallelism")).toBeTruthy();
    expect(screen.getByText("B — observed squashed blocks")).toBeTruthy();
    expect(screen.getAllByText("layer-source-a").length).toBeGreaterThan(0);
    expect(screen.getAllByText("layer-source-b").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Unavailable").length).toBeGreaterThan(0);
    expect(screen.getAllByText(/allocated-byte counter unavailable for this snapshot/).length).toBeGreaterThan(0);
    for (const snapshot of ["S0", "S1", "S2", "S3"]) {
      expect(screen.getByText(snapshot)).toBeTruthy();
    }
  });
});
