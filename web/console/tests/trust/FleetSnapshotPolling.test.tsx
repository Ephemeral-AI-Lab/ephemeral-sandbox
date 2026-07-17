import { act, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SandboxSnapshot, SnapshotResult } from "@/api/observability";
import type { SandboxRecord } from "@/api/types";
import { useFleetSnapshots } from "@/poll/useFleetSnapshots";

const mocks = vi.hoisted(() => ({
  fetchSandboxSnapshot: vi.fn(),
}));

vi.mock("@/api/observability", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/observability")>()),
  fetchSandboxSnapshot: mocks.fetchSandboxSnapshot,
}));

function record(id: string, revision = 0): SandboxRecord {
  return {
    id,
    workspace_root: `/work/${id}`,
    state: "ready",
    daemon: null,
    daemon_http: null,
    shared_base: null,
    activity_revision: revision,
  };
}

function snapshot(id: string, active = false, sampledAt = 1): SnapshotResult {
  const sandbox: SandboxSnapshot = {
    sandbox_id: id,
    lifecycle_state: "ready",
    availability: "available",
    sampled_at_unix_ms: sampledAt,
    errors: [],
    daemon: null,
    resources: { latest: null, history: [] },
    workspaces: [
      {
        workspace_id: `workspace-${id}`,
        lifecycle_state: "active",
        network_profile: "shared",
        layers: { base_root_hash: "root", layer_count: 0 },
        namespace_fd_count: 0,
        resources: { latest: null, history: [] },
        active_namespace_executions: active
          ? [
              {
                namespace_execution_id: `execution-${id}`,
                operation: "exec_command",
                lifecycle_state: "running",
              },
            ]
          : [],
      },
    ],
    stack: { layer_count: 0, layers_bytes: 0, active_leases: 0 },
  };
  return { sandboxes: [sandbox] };
}

function Harness({ records }: { records: SandboxRecord[] }) {
  useFleetSnapshots(records);
  return null;
}

function createWrapper() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: Number.POSITIVE_INFINITY },
    },
  });
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

async function flush(milliseconds = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
  });
}

describe("revision-gated fleet snapshots", () => {
  const next = new Map<string, SnapshotResult>();

  beforeEach(() => {
    vi.useFakeTimers();
    next.clear();
    next.set("sandbox-a", snapshot("sandbox-a"));
    next.set("sandbox-b", snapshot("sandbox-b"));
    mocks.fetchSandboxSnapshot.mockReset();
    mocks.fetchSandboxSnapshot.mockImplementation(async (sandboxId: string) =>
      structuredClone(next.get(sandboxId)),
    );
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolves once and performs no idle fleet daemon polling", async () => {
    render(<Harness records={[record("sandbox-a"), record("sandbox-b")]} />, {
      wrapper: createWrapper(),
    });
    await flush();
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(2);

    await flush(30_000);
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(2);
  });

  it("contacts only the sandbox whose activity revision changed", async () => {
    const view = render(
      <Harness records={[record("sandbox-a"), record("sandbox-b")]} />,
      { wrapper: createWrapper() },
    );
    await flush();
    mocks.fetchSandboxSnapshot.mockClear();

    view.rerender(
      <Harness records={[record("sandbox-a", 1), record("sandbox-b")]} />,
    );
    await flush();

    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(1);
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledWith(
      "sandbox-a",
      expect.any(AbortSignal),
    );
  });

  it("polls a known active sandbox only until its first inactive snapshot", async () => {
    next.set("sandbox-a", snapshot("sandbox-a", true));
    render(<Harness records={[record("sandbox-a"), record("sandbox-b")]} />, {
      wrapper: createWrapper(),
    });
    await flush();
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(2);

    next.set("sandbox-a", snapshot("sandbox-a", false, 2));
    await flush(400);
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(3);
    expect(
      mocks.fetchSandboxSnapshot.mock.calls.filter(
        ([sandboxId]) => sandboxId === "sandbox-b",
      ),
    ).toHaveLength(1);

    await flush(5_000);
    expect(mocks.fetchSandboxSnapshot).toHaveBeenCalledTimes(3);
  });
});
