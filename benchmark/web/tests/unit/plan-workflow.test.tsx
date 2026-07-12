import { MantineProvider } from "@mantine/core";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { benchmarkApi } from "@/api/client";
import type {
  DefinitionsResponse,
  ExperimentPlan,
  FactorDefinition,
  HealthResponse,
  PlanValidationResponse,
} from "@/api/types";
import { DefaultPlanLauncher, PlanReviewModal } from "@/components/DefaultPlanLauncher";
import { hasCompleteUiDefinitions } from "@/components/OperationControls";
import { benchmarkTheme } from "@/theme";

const factors: FactorDefinition[] = [
  {
    id: "concurrent_requests",
    label: "Concurrent requests",
    help: "Requests released through one barrier.",
    value_kind: "unsigned_integer",
    unit: "count",
    constraint: { kind: "positive" },
    comparison: "scientific_invariant",
  },
  {
    id: "workspace_profile",
    label: "Workspace profile",
    help: "Deterministic fixture profile.",
    value_kind: "choice",
    unit: null,
    constraint: { kind: "profile_catalog", catalog: "workspace_profiles" },
    comparison: "scientific_invariant",
  },
  {
    id: "session_mode",
    label: "Session boundary",
    help: "Explicit or automatic session lifecycle.",
    value_kind: "choice",
    unit: null,
    constraint: { kind: "choices", values: ["explicit", "automatic"] },
    comparison: "scientific_invariant",
  },
  {
    id: "command_case",
    label: "Command case",
    help: "Registered bounded shell template.",
    value_kind: "choice",
    unit: null,
    constraint: { kind: "choices", values: ["noop", "fixture_read"] },
    comparison: "scientific_invariant",
  },
];

function commandPlan(name = "standard-local"): ExperimentPlan {
  return {
    schema_version: 1,
    name,
    configuration_base: { id: "standard-local", version: 1, scope: "command" },
    seed: 20260712,
    environment: { image: "ubuntu:24.04", client_cohort: "direct_client" },
    protocol: {
      order: "randomized_blocks",
      resource_interval_ms: 100,
      trial_defaults: {
        fast: { warmups: 2, measured_trials: 30 },
        destructive: { warmups: 1, measured_trials: 10 },
      },
      timeout_ms: { default: 120_000, squash_layerstack: 600_000 },
    },
    operations: [
      {
        operation: "exec_command",
        configuration: {
          enabled: true,
          factors: {
            concurrent_requests: { role: "varied", values: [1, 5], control: 1 },
            workspace_profile: { role: "controlled", values: ["small"], control: null },
            session_mode: { role: "controlled", values: ["explicit"], control: null },
            command_case: { role: "controlled", values: ["noop"], control: null },
          },
        },
      },
    ],
    comparison: null,
  };
}

const operationDefinition: DefinitionsResponse["catalog"]["operations"][number] = {
  id: "exec_command",
  family: "command",
  label: "Execute command",
  help: "Execute one registered command workload.",
  measured_boundary: "Command request admission and execution.",
  count_semantics_help: "Concurrent requests are independent product requests released through one barrier.",
  semantic_revision: 1,
  factor_schema_revision: 1,
  count_semantics: { kind: "concurrent_requests", factor: "concurrent_requests" },
  execution_shape: "barrier_request_batch",
  isolation: "session_mode_dependent",
  cleanup: "resolve_from_isolation",
  product_access: { kind: "public_gateway", action: "exec_command" },
  supported_cohorts: ["direct_client", "cli_e2e"],
  security_class: "bounded_shell",
  factors,
  checks: [],
  phases: [],
  comparison: {
    semantic_revision: 1,
    factors: ["concurrent_requests", "workspace_profile", "session_mode", "command_case"],
  },
};

const defaultPlan = commandPlan();
const quickSmoke = commandPlan("quick-smoke");
const definitions: DefinitionsResponse = {
  schema_version: 1,
  catalog: {
    schema_version: 1,
    families: [
      {
        id: "command",
        label: "Command",
        help: "Command benchmarks.",
        research_question: "How does concurrency affect command latency?",
        measured_boundary: "One command request.",
      },
    ],
    factor_roles: ["varied", "controlled"],
    workspace_profiles: {
      schema_version: 1,
      profiles: [
        {
          schema_version: 1,
          id: "small",
          version: 1,
          label: "Small",
          help: "Small deterministic fixture.",
          generator_version: 1,
          standard: true,
          fixture: { file_count: 1000, logical_bytes: 16_777_216, maximum_depth: 4 },
        },
      ],
    },
    operations: [operationDefinition],
    metrics: [],
  },
  defaults: [defaultPlan],
  presets: [{ schema_version: 1, id: "quick-smoke", version: 1, plan: quickSmoke }],
};

function validationFor(plan: ExperimentPlan, hash = `hash-${plan.name}`): PlanValidationResponse {
  return {
    schema_version: 1,
    runnable: true,
    is_customized: plan.name !== "standard-local",
    plan_hash: hash,
    canonical_plan: plan,
    effective_environment: {
      test_workspace_root: "/tmp/eos-benchmark",
      workspace_root_identity: "workspace-id",
      client_cohort: "direct_client",
      image_digest: "sha256:image",
      filesystem: "ext4",
      free_space_bytes: 10_000_000,
      gateway_mode: "isolated",
    },
    fixed_lifecycle_policy: {
      lifecycle_revision: 1,
      failure_revision: 1,
      stabilization_revision: 1,
      automatic_retries: 0,
      one_active_campaign: true,
      sequential_families: true,
    },
    selected_workspace_profiles: definitions.catalog.workspace_profiles.profiles,
    cells: [
      {
        cell_id: "cell-command-1",
        family_id: "command",
        operation_id: "exec_command",
        operation_semantic_revision: 1,
        factor_schema_revision: 1,
        protocol: {
          destructive: false,
          warmups: 2,
          measured_trials: 30,
          timeout_ms: 120_000,
          cleanup: "verify_fixture_unchanged",
        },
        operation: {
          operation: "exec_command",
          cell: {
            concurrent_requests: 1,
            workspace_profile: "small",
            session_mode: "explicit",
            command_case: "noop",
            template_revision: 1,
            command: "true",
            command_sha256: "command-hash",
            expected_exit_code: 0,
            output_limit_bytes: 4096,
            resolved_isolation: "reusable_verified_fixture",
          },
        },
      },
    ],
    execution_blocks: [
      { block_id: "block-command", family_id: "command", cell_ids: ["cell-command-1"], restart_reason: null },
    ],
    estimates: {
      cell_count: 2,
      trial_batch_count: 64,
      issued_operation_request_count: 192,
      duration_range: { minimum_ns: 1_000_000, maximum_ns: 5_000_000 },
      estimated_peak_disk_bytes: 16_777_216,
      required_free_space_bytes: 33_554_432,
      gateway_restart_count: 0,
      warnings: [],
    },
    validation: [],
  };
}

const readyHealth: HealthResponse = {
  schema_version: 1,
  status: "ready",
  execution_ready: true,
  version: "0.1.0",
  runner_instance_id: "runner-1",
  active_run: null,
  checks: [],
};

function renderWithProviders(node: React.ReactNode) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <MantineProvider theme={benchmarkTheme}>
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/benchmark/command"]}>{node}</MemoryRouter>
      </QueryClientProvider>
    </MantineProvider>,
  );
}

afterEach(() => vi.restoreAllMocks());

describe("typed plan workflow", () => {
  it("fails the explicit UI registration check when required factor metadata is missing", () => {
    expect(hasCompleteUiDefinitions(defaultPlan.operations, definitions.catalog)).toBe(true);
    const incomplete = structuredClone(definitions.catalog);
    incomplete.operations[0]!.factors = factors.filter(({ id }) => id !== "command_case");
    expect(hasCompleteUiDefinitions(defaultPlan.operations, incomplete)).toBe(false);

    const unexpected = structuredClone(definitions.catalog);
    unexpected.operations[0]!.factors.push({ ...factors[0]!, id: "returned_bytes" });
    expect(hasCompleteUiDefinitions(defaultPlan.operations, unexpected)).toBe(false);
  });

  it("starts with the exact reviewed canonical plan hash and keeps the three work counts distinct", async () => {
    const createRun = vi.spyOn(benchmarkApi, "createRun").mockResolvedValue({
      schema_version: 1,
      run_id: "run-created",
      state: "queued",
    });
    const validation = validationFor(defaultPlan, "server-authored-plan-hash");
    renderWithProviders(
      <PlanReviewModal
        opened
        close={() => {}}
        validation={validation}
        health={readyHealth}
        healthPending={false}
        healthError={null}
        retryHealth={() => {}}
        startingPreset={{ id: "quick-smoke", version: 1 }}
      />,
    );

    expect(screen.getByText("Test combinations")).toBeTruthy();
    expect(screen.getByText("Trial batches")).toBeTruthy();
    expect(screen.getByText("Issued product requests")).toBeTruthy();
    expect(screen.getByTestId("review-plan-hash").textContent).toBe("server-authored-plan-hash");
    await userEvent.click(screen.getByRole("button", { name: "Start local run" }));

    await waitFor(() => expect(createRun).toHaveBeenCalledTimes(1));
    expect(createRun.mock.calls[0]?.[0]).toMatchObject({
      plan: validation.canonical_plan,
      plan_hash: "server-authored-plan-hash",
      starting_preset: { id: "quick-smoke", version: 1 },
    });
  });

  it("blocks start while execution readiness is false", () => {
    renderWithProviders(
      <PlanReviewModal
        opened
        close={() => {}}
        validation={validationFor(defaultPlan)}
        health={{ ...readyHealth, status: "unready", execution_ready: false }}
        healthPending={false}
        healthError={null}
        retryHealth={() => {}}
        startingPreset={null}
      />,
    );

    const start = screen.getByRole("button", { name: "Start local run" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    expect(screen.getByText("Runner admission is not ready")).toBeTruthy();
  });

  it("loads a server-authored preset and exposes explicit command controls", async () => {
    vi.spyOn(benchmarkApi, "definitions").mockResolvedValue(definitions);
    vi.spyOn(benchmarkApi, "health").mockResolvedValue(readyHealth);
    const validatePlan = vi.spyOn(benchmarkApi, "validatePlan").mockImplementation(async ({ plan }) => validationFor(plan));
    renderWithProviders(<DefaultPlanLauncher scope="command" />);

    await screen.findByText("standard-local");
    expect(await screen.findByText("Estimated duration")).toBeTruthy();
    expect(screen.getByText("Required free space")).toBeTruthy();
    expect(screen.getByText("Changes across test combinations")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Customize" }));
    expect(await screen.findByText("Allowlisted bounded shell case")).toBeTruthy();
    expect(screen.getByText("Concurrent requests")).toBeTruthy();
    expect(screen.getByText("Session boundary")).toBeTruthy();

    await userEvent.click(screen.getByRole("combobox", { name: "Preset" }));
    await userEvent.click(await screen.findByText("quick-smoke · v1"));
    await userEvent.click(screen.getByRole("button", { name: "Load preset" }));

    expect(await screen.findByText("Preset quick-smoke/v1")).toBeTruthy();
    await waitFor(() =>
      expect(validatePlan).toHaveBeenCalledWith(
        expect.objectContaining({ starting_preset: { id: "quick-smoke", version: 1 } }),
      ),
    );
  });

  it("offers only client cohorts advertised by every enabled operation", async () => {
    const directOnlyDefinitions = structuredClone(definitions);
    directOnlyDefinitions.catalog.operations[0]!.supported_cohorts = ["direct_client"];
    vi.spyOn(benchmarkApi, "definitions").mockResolvedValue(directOnlyDefinitions);
    vi.spyOn(benchmarkApi, "health").mockResolvedValue(readyHealth);
    vi.spyOn(benchmarkApi, "validatePlan").mockImplementation(async ({ plan }) => validationFor(plan));
    renderWithProviders(<DefaultPlanLauncher scope="command" />);

    await screen.findByText("standard-local");
    await userEvent.click(screen.getByRole("button", { name: "Customize" }));
    const cohort = screen.getByRole("combobox", { name: "Client cohort" }) as HTMLInputElement;

    expect(cohort.value).toBe("Direct client");
    expect(screen.queryByText("CLI end to end")).toBeNull();
  });
});
