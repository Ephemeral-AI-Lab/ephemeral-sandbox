import type { ConfigurationScope, FamilyId } from "@/api/types";

export type FamilyRouteId = "command" | "files" | "workspace" | "layerstack";

export const FAMILY_ROUTES = {
  command: {
    path: "/benchmark/command",
    fallbackLabel: "Command",
    familyId: "command",
    scope: "command",
  },
  files: {
    path: "/benchmark/files",
    fallbackLabel: "File Operations",
    familyId: "files",
    scope: "files",
  },
  workspace: {
    path: "/benchmark/workspace",
    fallbackLabel: "Workspace Lifecycle",
    familyId: "workspace_lifecycle",
    scope: "workspace",
  },
  layerstack: {
    path: "/benchmark/layerstack",
    fallbackLabel: "LayerStack",
    familyId: "layer_stack",
    scope: "layerstack",
  },
} as const satisfies Record<
  FamilyRouteId,
  {
    path: `/benchmark/${string}`;
    fallbackLabel: string;
    familyId: FamilyId;
    scope: ConfigurationScope;
  }
>;

export const ROUTE_PATTERNS = [
  "/benchmark",
  "/benchmark/command",
  "/benchmark/files",
  "/benchmark/workspace",
  "/benchmark/layerstack",
  "/benchmark/runs/:runId",
  "/benchmark/reports/:runId",
  "/benchmark/compare",
] as const;
