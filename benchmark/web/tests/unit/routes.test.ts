import { describe, expect, it } from "vitest";
import { FAMILY_ROUTES, ROUTE_PATTERNS } from "@/routes";

describe("benchmark route contract", () => {
  it("contains exactly the eight documented route shapes", () => {
    expect(ROUTE_PATTERNS).toEqual([
      "/benchmark",
      "/benchmark/command",
      "/benchmark/files",
      "/benchmark/workspace",
      "/benchmark/layerstack",
      "/benchmark/runs/:runId",
      "/benchmark/reports/:runId",
      "/benchmark/compare",
    ]);
  });

  it("keeps the LayerStack route scope distinct from its FamilyId wire value", () => {
    expect(FAMILY_ROUTES.layerstack).toMatchObject({
      path: "/benchmark/layerstack",
      scope: "layerstack",
      familyId: "layer_stack",
    });
  });
});
