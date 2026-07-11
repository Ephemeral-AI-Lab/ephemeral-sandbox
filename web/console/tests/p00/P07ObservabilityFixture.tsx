import { useMemo } from "react";
import { createRoot } from "react-dom/client";
import { Box, MantineProvider } from "@mantine/core";
import { Notifications } from "@mantine/notifications";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Outlet, Route, Routes } from "react-router";
import { EventsView } from "../../src/pages/sandbox/observability/EventsView";
import { LayerStackView } from "../../src/pages/sandbox/observability/LayerStackView";
import { ResourcesView } from "../../src/pages/sandbox/observability/ResourcesView";
import { TracesView } from "../../src/pages/sandbox/observability/TracesView";
import { ephemeralosTheme } from "../../src/theme";
import "@mantine/core/styles.css";
import "@mantine/notifications/styles.css";
import "../../src/index.css";

const sandboxId = "observability-fixture";
const view = new URL(window.location.href).searchParams.get("view") ?? "events";

const snapshot = {
  sandboxes: [{
    sandbox_id: sandboxId,
    lifecycle_state: "ready",
    availability: "available",
    sampled_at_unix_ms: 1_700_000_000_000,
    errors: [],
    daemon: null,
    resources: { latest: null, history: [] },
    workspaces: [{
      workspace_id: "workspace-fixture",
      lifecycle_state: "running",
      network_profile: "shared",
      layers: { base_root_hash: "fixture-base", layer_count: 3 },
      namespace_fd_count: 1,
      resources: { latest: null, history: [] },
      active_namespace_executions: [],
    }],
    stack: { layer_count: 3, layers_bytes: 6_000_000, active_leases: 1 },
  }],
};

function SandboxContext() {
  return <Outlet context={{ sandboxId, record: null, snapshot, recordError: null }} />;
}

function Fixture() {
  const queryClient = useMemo(
    () => new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } }),
    [],
  );
  return (
    <MantineProvider forceColorScheme="light" theme={ephemeralosTheme}>
      <Notifications limit={4} position="bottom-right" />
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[`/${view}`]}>
          <Box component="main" h="100%">
            <Routes>
              <Route element={<SandboxContext />}>
                <Route path="/events" element={<EventsView />} />
                <Route path="/resources" element={<ResourcesView />} />
                <Route path="/traces" element={<TracesView />} />
                <Route path="/traces/:traceId" element={<TracesView />} />
                <Route path="/layers" element={<LayerStackView />} />
              </Route>
            </Routes>
          </Box>
        </MemoryRouter>
      </QueryClientProvider>
    </MantineProvider>
  );
}

createRoot(document.getElementById("root")!).render(<Fixture />);
