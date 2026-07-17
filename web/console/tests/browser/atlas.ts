import type { Page } from "@playwright/test";

export const SANDBOX_ID = "fixture-sandbox";
const SAMPLE_TIME = 1_700_000_000_000;

const record = {
  id: SANDBOX_ID,
  workspace_root: "/fixture/workspace",
  state: "ready",
  daemon: { host: "127.0.0.1", port: 7801 },
  daemon_http: { host: "127.0.0.1", port: 7802 },
  shared_base: {
    source: "fixture-image",
    target: "/fixture/base",
    root_hash: "fixture-base-root-hash",
    readonly: true,
  },
};

const samples = Array.from({ length: 12 }, (_, index) => ({
  ts: SAMPLE_TIME - (11 - index) * 5_000,
  sample_delta_ms: 5_000,
  metrics: {
    mem_cur: 24_000_000 + index * 50_000,
    disk_bytes: 4_000_000 + index * 1_000,
    files: 17,
    cgroup_available: true,
  },
  deltas: {
    cpu_usec: 10_000 + index * 500,
    io_rbytes: 500 + index * 10,
    io_wbytes: 200 + index * 5,
  },
}));

const snapshot = {
  sandboxes: [
    {
      sandbox_id: SANDBOX_ID,
      lifecycle_state: "ready",
      availability: "available",
      sampled_at_unix_ms: SAMPLE_TIME,
      errors: [],
      daemon: { daemon_pid: 42, runtime_dir: "/fixture/runtime" },
      resources: { latest: samples.at(-1), history: samples },
      workspaces: [
        {
          workspace_id: "workspace-fixture",
          lifecycle_state: "running",
          network_profile: "shared",
          layers: { base_root_hash: "fixture-base-root-hash", layer_count: 3 },
          namespace_fd_count: 5,
          resources: { latest: samples.at(-1), history: samples },
          active_namespace_executions: [],
        },
      ],
      stack: { layer_count: 3, layers_bytes: 6_000_000, active_leases: 1 },
    },
  ],
};

const topology = {
  schema_version: 2,
  available: true,
  source: "proc_namespaces",
  error: null,
  truncated: false,
  warnings: [],
  workspaces: [
    {
      workspace_id: "workspace-fixture",
      state: "active",
      holder_pid: 101,
      pid_namespace: "pid:[1001]",
      mount_namespace: "mnt:[2001]",
      processes: [
        { pid: 201, namespace_pid: 1, parent_pid: 101, name: "ns-init", state: "S (sleeping)", kind: "namespace_init", cgroup_memberships: ["0::/"] },
        { pid: 233, namespace_pid: 2, parent_pid: 201, name: "bash", state: "S (sleeping)", kind: "process", cgroup_memberships: ["0::/"] },
      ],
    },
  ],
};

const events = {
  view: "events",
  events: [
    {
      ts: SAMPLE_TIME,
      trace: "trace-fixture",
      parent: null,
      name: "workspace.published",
      attrs: { revision: "fixture-r1", fixture: true },
    },
  ],
};

export async function installAtlasApi(page: Page) {
  await page.route(`**/api/sandboxes/${SANDBOX_ID}/health`, (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ status: "ok" }) }),
  );
  await page.route(`**/api/sandboxes/${SANDBOX_ID}/files/list`, (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        path: "",
        entries: [{ name: "notes", kind: "directory", size: null }, { name: "operator.txt", kind: "file", size: 42 }],
        truncated: false,
      }),
    }),
  );
  await page.route("**/api/rpc", async (route) => {
    const { op, args, scope } = route.request().postDataJSON() as {
      op: string;
      args: Record<string, unknown>;
      scope: { kind?: string };
    };
    let body: unknown;
    switch (op) {
      case "list_sandboxes":
        body = { sandboxes: [record] };
        break;
      case "inspect_sandbox":
        body = record;
        break;
      case "snapshot":
        body = scope.kind === "sandbox" ? snapshot.sandboxes[0] : snapshot;
        break;
      case "cgroup":
        body = { view: "cgroup", scope: String(args.scope ?? "sandbox"), series: samples, topology };
        break;
      case "events":
        body = events;
        break;
      case "trace":
        body = {
          view: "trace",
          trace: "trace-fixture",
          spans: [
            {
              span: { ts: SAMPLE_TIME - 500, trace: "trace-fixture", span: "root", name: "fixture.operation", dur_ms: 500, status: "ok", attrs: { fixture: true } },
              offset_ms: 0,
              children: [],
              events: [{ offset_ms: 200, event: events.events[0] }],
            },
          ],
        };
        break;
      case "layerstack":
        body = args.layer_id
          ? { view: "layerstack", layer_id: args.layer_id, entries: [{ path: "notes/operator.txt", kind: "file" }], truncated: false }
          : args.workspace_id
            ? { view: "layerstack", workspace: args.workspace_id, mounts: [], upper_bytes: 1_000 }
            : {
                view: "layerstack",
                manifest_version: 1,
                root_hash: "fixture-root-hash",
                active_lease_count: 1,
                total_bytes: 6_000_000,
                layers: [
                  { layer_id: "fixture-layer-2", bytes: 3_000_000, leased_by_workspaces: 0, booked_by: [] },
                  { layer_id: "fixture-layer-1", bytes: 2_000_000, leased_by_workspaces: 1, booked_by: ["workspace-fixture"] },
                  { layer_id: "fixture-base", bytes: 1_000_000, leased_by_workspaces: 0, booked_by: [] },
                ],
              };
        break;
      case "file_read":
        body = {
          path: String(args.path ?? "operator.txt"),
          content: "fixture operator note\nthis deterministic file contains no live data",
          start_line: 1,
          num_lines: 2,
          total_lines: 2,
          bytes_read: 64,
          total_bytes: 64,
          next_offset: null,
          truncated: false,
        };
        break;
      default:
        await route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: { kind: "fixture_error", message: `unexpected operation ${op}` } }) });
        return;
    }
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
}

export async function waitForAtlasRoute(page: Page, ready: string) {
  await page.getByText(ready, { exact: false }).waitFor();
  await page.waitForFunction(() => {
    const logo = document.querySelector('img[src="/assets/images/logo.png"]');
    return logo instanceof HTMLImageElement && logo.complete && logo.naturalWidth > 0;
  });
}

export const atlasRoutes = [
  { name: "fleet", path: "/", ready: "fixture-sandbox" },
  { name: "terminal", path: `/sandboxes/${SANDBOX_ID}/terminal`, ready: "No commands yet" },
  { name: "resources", path: `/sandboxes/${SANDBOX_ID}/observability/resources`, ready: "CPU (Δ cpu_usec / s)" },
  { name: "cgroup", path: `/sandboxes/${SANDBOX_ID}/observability/cgroup`, ready: "Workspace process topology" },
  { name: "events", path: `/sandboxes/${SANDBOX_ID}/observability/events`, ready: "workspace.published" },
  { name: "traces", path: `/sandboxes/${SANDBOX_ID}/observability/traces`, ready: "fixture.operation" },
  { name: "layers", path: `/sandboxes/${SANDBOX_ID}/layerstack`, ready: "fixture-layer-2" },
  { name: "files", path: `/sandboxes/${SANDBOX_ID}/files?path=operator.txt`, ready: "fixture operator note" },
  { name: "preview", path: `/sandboxes/${SANDBOX_ID}/preview`, ready: "Pick a port to preview" },
] as const;

export const atlasViewports = [
  { name: "375x812", width: 375, height: 812 },
  { name: "768x1024", width: 768, height: 1024 },
  { name: "1024x768", width: 1024, height: 768 },
  { name: "1440x900", width: 1440, height: 900 },
] as const;
