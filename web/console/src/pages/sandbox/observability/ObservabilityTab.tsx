import { Box, Tabs } from "@mantine/core";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useSandbox } from "@/pages/sandbox/SandboxContext";

const VIEWS = [
  { path: "resources", label: "Resources" },
  { path: "cgroup", label: "Processes" },
  { path: "traces", label: "Traces" },
  { path: "events", label: "Events" },
  { path: "layerstack", label: "Layers" },
];

/**
 * Observability sub-navigation mirrors the observability catalog's
 * per-sandbox views. Bare `/observability` redirects to `resources` in the
 * route tree, and all tabs retain directly addressable paths.
 */
export function ObservabilityTab() {
  const sandbox = useSandbox();
  const location = useLocation();
  const navigate = useNavigate();
  const currentView = VIEWS.find(
    (view) =>
      location.pathname.endsWith(`/${view.path}`) || location.pathname.includes(`/${view.path}/`),
  )?.path ?? "resources";

  return (
    <Box data-observability-tab>
      <Tabs
        onChange={(path) => {
          if (path) void navigate(path);
        }}
        value={currentView}
        variant="outline"
      >
        <Tabs.List aria-label="Observability navigation" data-observability-tabs>
          {VIEWS.map((view) => (
            <Tabs.Tab key={view.path} value={view.path}>
              {view.label}
            </Tabs.Tab>
          ))}
        </Tabs.List>
      </Tabs>
      <Box data-observability-content>
        <Outlet context={sandbox} />
      </Box>
    </Box>
  );
}
