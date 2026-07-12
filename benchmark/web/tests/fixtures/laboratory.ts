import type {
  Availability,
  ComparisonResponse,
  ConfigurationScope,
  DefinitionsResponse,
  EventData,
  EventRecord,
  ExperimentPlan,
  FactorDefinition,
  FamilyId,
  HealthResponse,
  OperationDefinition,
  OperationEvidenceReport,
  OperationId,
  OperationObservationRecord,
  OperationPlan,
  PlanValidationResponse,
  ReportResponse,
  RunListResponse,
  RunResponse,
  RunState,
  SampleStatistics,
  SettingsResponse,
  StorageSnapshot,
} from "@/api/types";

export const UI_FIXTURE_NAMES = [
  "overview-default-ready",
  "overview-runner-unavailable",
  "overview-root-unwritable",
  "command-default",
  "command-customize-unchanged",
  "command-customized",
  "command-validation-updating",
  "command-validation-error",
  "command-reset-to-default",
  "command-allowlisted-shell-case",
  "files-publish-warning",
  "workspace-large-insufficient-space",
  "layerstack-n0-control",
  "layerstack-remount-restarts",
  "run-running-setup",
  "run-running-operation",
  "run-reconnecting",
  "run-cancelling",
  "run-correctness-failed",
  "report-complete-n29",
  "report-complete-n30",
  "report-partial-unavailable-resource",
  "report-cpu-latency-correlation",
  "report-old-definition-snapshot",
  "compare-compatible",
  "compare-incompatible",
  "compare-descriptive-override",
] as const;

export type UiFixtureName = (typeof UI_FIXTURE_NAMES)[number];

export const FIXTURE_ROUTE: Record<UiFixtureName, string> = {
  "overview-default-ready": "/benchmark",
  "overview-runner-unavailable": "/benchmark",
  "overview-root-unwritable": "/benchmark",
  "command-default": "/benchmark/command",
  "command-customize-unchanged": "/benchmark/command",
  "command-customized": "/benchmark/command",
  "command-validation-updating": "/benchmark/command",
  "command-validation-error": "/benchmark/command",
  "command-reset-to-default": "/benchmark/command",
  "command-allowlisted-shell-case": "/benchmark/command",
  "files-publish-warning": "/benchmark/files",
  "workspace-large-insufficient-space": "/benchmark/workspace",
  "layerstack-n0-control": "/benchmark/layerstack",
  "layerstack-remount-restarts": "/benchmark/layerstack",
  "run-running-setup": "/benchmark/runs/run-fixture",
  "run-running-operation": "/benchmark/runs/run-fixture",
  "run-reconnecting": "/benchmark/runs/run-fixture",
  "run-cancelling": "/benchmark/runs/run-fixture",
  "run-correctness-failed": "/benchmark/runs/run-fixture",
  "report-complete-n29": "/benchmark/reports/run-reference",
  "report-complete-n30": "/benchmark/reports/run-reference",
  "report-partial-unavailable-resource": "/benchmark/reports/run-reference",
  "report-cpu-latency-correlation": "/benchmark/reports/run-reference?view=resources",
  "report-old-definition-snapshot": "/benchmark/reports/run-reference?view=methods",
  "compare-compatible": "/benchmark/compare",
  "compare-incompatible": "/benchmark/compare",
  "compare-descriptive-override": "/benchmark/compare",
};

const familyByOperation = {
  exec_command: "command",
  file_read: "files",
  file_write: "files",
  file_edit: "files",
  file_blame: "files",
  create_workspace: "workspace_lifecycle",
  squash_layerstack: "layer_stack",
} as const satisfies Record<OperationId, FamilyId>;

const scopeByFamily = {
  command: "command",
  files: "files",
  workspace_lifecycle: "workspace",
  layer_stack: "layerstack",
} as const satisfies Record<FamilyId, Exclude<ConfigurationScope, "all">>;

function choiceFactor(id: FactorDefinition["id"], values: string[], label = id.replaceAll("_", " ")): FactorDefinition {
  return {
    id,
    label,
    help: `Versioned ${label} boundary.`,
    value_kind: "choice",
    unit: null,
    constraint: { kind: "choices", values },
    comparison: "scientific_invariant",
  };
}

function numericFactor(
  id: FactorDefinition["id"],
  constraint: "positive" | "non_negative" | "unit_interval",
  unit: "count" | "bytes" | "ratio",
): FactorDefinition {
  return {
    id,
    label: id.replaceAll("_", " "),
    help: `Versioned ${id.replaceAll("_", " ")} boundary.`,
    value_kind: constraint === "unit_interval" ? "unit_ratio" : "unsigned_integer",
    unit,
    constraint: { kind: constraint },
    comparison: "scientific_invariant",
  };
}

const workspaceProfileFactor: FactorDefinition = {
  id: "workspace_profile",
  label: "Workspace profile",
  help: "A deterministic versioned workspace fixture.",
  value_kind: "choice",
  unit: null,
  constraint: { kind: "profile_catalog", catalog: "workspace_profiles" },
  comparison: "scientific_invariant",
};

const operationFactors = {
  exec_command: [
    numericFactor("concurrent_requests", "positive", "count"),
    workspaceProfileFactor,
    choiceFactor("session_mode", ["explicit", "automatic"]),
    choiceFactor("command_case", ["noop", "output64_kib", "cpu50_ms", "fixture_read"]),
  ],
  file_read: [
    numericFactor("concurrent_requests", "positive", "count"),
    numericFactor("returned_bytes", "positive", "bytes"),
    choiceFactor("read_source", ["snapshot", "session"]),
    choiceFactor("target_mode", ["independent", "same_target"]),
  ],
  file_write: [
    numericFactor("concurrent_requests", "positive", "count"),
    numericFactor("content_bytes", "positive", "bytes"),
    choiceFactor("mutation_destination", ["session", "publish"]),
    choiceFactor("target_mode", ["independent", "same_target"]),
  ],
  file_edit: [
    numericFactor("concurrent_requests", "positive", "count"),
    numericFactor("file_bytes", "positive", "bytes"),
    numericFactor("replacement_count", "positive", "count"),
    numericFactor("match_density", "unit_interval", "ratio"),
    choiceFactor("mutation_destination", ["session", "publish"]),
    choiceFactor("target_mode", ["independent", "same_target"]),
  ],
  file_blame: [
    numericFactor("concurrent_requests", "positive", "count"),
    numericFactor("line_count", "positive", "count"),
    numericFactor("ownership_segments", "positive", "count"),
    numericFactor("auditability_event_count", "non_negative", "count"),
  ],
  create_workspace: [
    numericFactor("workspace_count", "positive", "count"),
    workspaceProfileFactor,
    choiceFactor("network_profile", ["shared", "isolated"]),
  ],
  squash_layerstack: [
    numericFactor("live_sessions", "non_negative", "count"),
    numericFactor("requested_migration_ratio", "unit_interval", "ratio"),
    numericFactor("remount_parallelism", "positive", "count"),
    numericFactor("squashable_blocks", "positive", "count"),
    numericFactor("layers_per_block", "positive", "count"),
    numericFactor("payload_bytes", "positive", "bytes"),
    choiceFactor("session_activity", ["idle", "active"]),
  ],
} as const satisfies Record<OperationId, readonly FactorDefinition[]>;

export const LAYERSTACK_PHASES: OperationDefinition["phases"] = [
  { id: "layerstack_squash", label: "Total squash", help: "Server-observed total duration of the one squash request.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "layerstack.squash" },
  { id: "layerstack_storage_plan", label: "Storage plan", help: "Server phase that plans the layer-storage rewrite.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "layerstack.squash.plan" },
  { id: "layerstack_flatten", label: "Flatten", help: "Server phase that materializes flattened layer content.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "layerstack.squash.flatten" },
  { id: "layerstack_commit", label: "Commit", help: "Server phase that atomically commits the new manifest.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "layerstack.squash.commit" },
  { id: "layerstack_remount_sweep", label: "Remount sweep", help: "Wall time of the bounded post-commit session sweep.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "layerstack.squash.remount_sweep" },
  { id: "workspace_session_remount", label: "Session remount", help: "Per-session remount span observed within the bounded sweep.", semantic_revision: 1, unit: "nanoseconds", source: "product_trace", correlation: "exact_request_trace_span", trace_span_name: "workspace_session.remount" },
];

function operationDefinition(id: OperationId): OperationDefinition {
  const family = familyByOperation[id];
  const concurrency = id === "create_workspace"
    ? { kind: "concurrent_workspace_creates" as const, factor: "workspace_count" as const }
    : id === "squash_layerstack"
      ? { kind: "single_request_with_prepared_load" as const, load_factor: "live_sessions" as const }
      : { kind: "concurrent_requests" as const, factor: "concurrent_requests" as const };
  const productAccess: OperationDefinition["product_access"] = id === "create_workspace"
    ? { kind: "internal_workspace", action: "create_no_op_session" }
    : { kind: "public_gateway", action: id === "squash_layerstack" ? "squash_layerstacks" : id };
  return {
    id,
    family,
    label: id.replaceAll("_", " "),
    help: `Fixed ${id.replaceAll("_", " ")} operation definition.`,
    measured_boundary: "One typed product boundary.",
    count_semantics_help: id === "squash_layerstack" ? "One request after preparing N live sessions." : "Independent requests released through one barrier.",
    semantic_revision: 1,
    factor_schema_revision: 1,
    count_semantics: concurrency,
    execution_shape: id === "create_workspace" ? "barrier_workspace_creation" : id === "squash_layerstack" ? "single_request_after_prepared_load" : "barrier_request_batch",
    isolation: id === "squash_layerstack" ? "fresh_topology_per_trial" : id === "create_workspace" ? "prepared_sandbox_per_cell" : id === "exec_command" ? "session_mode_dependent" : id === "file_read" || id === "file_blame" ? "reusable_verified_fixture" : "mutation_destination_dependent",
    cleanup: id === "squash_layerstack" ? "destroy_topology_and_verify_baseline" : id === "create_workspace" ? "destroy_sessions_and_verify_baseline" : id === "file_read" || id === "file_blame" ? "verify_fixture_unchanged" : "resolve_from_isolation",
    product_access: productAccess,
    supported_cohorts: ["direct_client"],
    security_class: id === "exec_command" ? "bounded_shell" : id === "create_workspace" ? "internal_workspace_lifecycle" : id === "squash_layerstack" ? "destructive_manager_mutation" : id === "file_read" || id === "file_blame" ? "public_read_only" : "public_mutation",
    factors: [...operationFactors[id]],
    checks: [],
    phases: id === "squash_layerstack" ? structuredClone(LAYERSTACK_PHASES) : [],
    comparison: { semantic_revision: 1, factors: operationFactors[id].map(({ id: factorId }) => factorId) },
  };
}

const controlled = <T,>(value: T) => ({ role: "controlled" as const, values: [value], control: null });

export const ALL_OPERATION_PLANS: OperationPlan[] = [
  { operation: "exec_command", configuration: { enabled: true, factors: { concurrent_requests: controlled(1), workspace_profile: controlled("small"), session_mode: controlled("explicit"), command_case: controlled("noop") } } },
  { operation: "file_read", configuration: { enabled: true, factors: { concurrent_requests: controlled(1), returned_bytes: controlled(4096), source: controlled("snapshot"), target_mode: controlled("independent") } } },
  { operation: "file_write", configuration: { enabled: true, factors: { concurrent_requests: controlled(1), content_bytes: controlled(4096), destination: controlled("session"), target_mode: controlled("independent") } } },
  { operation: "file_edit", configuration: { enabled: true, factors: { concurrent_requests: controlled(1), file_bytes: controlled(4096), replacement_count: controlled(1), match_density: controlled(1), destination: controlled("session"), target_mode: controlled("independent") } } },
  { operation: "file_blame", configuration: { enabled: true, factors: { concurrent_requests: controlled(1), line_count: controlled(100), ownership_segments: controlled(10), auditability_event_count: controlled(10) } } },
  { operation: "create_workspace", configuration: { enabled: true, factors: { workspace_count: controlled(1), workspace_profile: controlled("small"), network_profile: controlled("shared") } } },
  { operation: "squash_layerstack", configuration: { enabled: true, factors: { live_sessions: controlled(0), requested_migration_ratio: controlled(1), remount_parallelism: controlled(4), squashable_blocks: controlled(1), layers_per_block: controlled(8), payload_bytes: controlled(4096), session_activity: controlled("idle") } } },
];

export function planForScope(scope: ConfigurationScope, name = "standard-local"): ExperimentPlan {
  const operations = ALL_OPERATION_PLANS.filter(({ operation }) =>
    scope === "all" || scopeByFamily[familyByOperation[operation]] === scope,
  );
  return {
    schema_version: 1,
    name,
    configuration_base: { id: "standard-local", version: 1, scope },
    seed: 20260712,
    environment: { image: "ubuntu:24.04", client_cohort: "direct_client" },
    protocol: {
      order: "randomized_blocks",
      resource_interval_ms: 100,
      trial_defaults: { fast: { warmups: 1, measured_trials: 5 }, destructive: { warmups: 1, measured_trials: 5 } },
      timeout_ms: { default: 120_000, squash_layerstack: 600_000 },
    },
    operations: structuredClone(operations),
    comparison: null,
  };
}

export const DEFINITIONS_FIXTURE: DefinitionsResponse = {
  schema_version: 1,
  catalog: {
    schema_version: 2,
    families: [
      { id: "command", label: "Command", help: "Command benchmarks.", research_question: "How does command work respond to load?", measured_boundary: "One command request." },
      { id: "files", label: "File Operations", help: "File benchmarks.", research_question: "How do file operations respond to load?", measured_boundary: "One file request." },
      { id: "workspace_lifecycle", label: "Workspace Lifecycle", help: "Workspace benchmarks.", research_question: "How does time to ready respond to load?", measured_boundary: "One create request." },
      { id: "layer_stack", label: "LayerStack", help: "LayerStack benchmarks.", research_question: "How does squash respond to live-session load?", measured_boundary: "One squash request." },
    ],
    factor_roles: ["varied", "controlled"],
    workspace_profiles: {
      schema_version: 1,
      profiles: [
        { schema_version: 1, id: "small", version: 1, label: "Small", help: "Deterministic small fixture.", generator_version: 1, standard: true, fixture: { file_count: 1000, logical_bytes: 16_777_216, maximum_depth: 4 } },
        { schema_version: 1, id: "large", version: 1, label: "Large", help: "Opt-in large fixture.", generator_version: 1, standard: false, fixture: { file_count: 50_000, logical_bytes: 2_147_483_648, maximum_depth: 12 } },
      ],
    },
    operations: (Object.keys(familyByOperation) as OperationId[]).map(operationDefinition),
    metrics: [
      { id: "sandbox_cpu_time_ns", semantic_revision: 1, unit: "nanoseconds", scope: "sandbox", kind: "monotonic_counter", availability: "explicit_unavailable", aggregation: "delta", direction: "lower_is_preferred" },
    ],
  },
  defaults: (["all", "command", "files", "workspace", "layerstack"] as ConfigurationScope[]).map((scope) => planForScope(scope)),
  presets: [{ schema_version: 1, id: "quick-smoke", version: 1, plan: planForScope("all", "quick-smoke") }],
};

export function validationFixture(plan: ExperimentPlan, scenario: UiFixtureName): PlanValidationResponse {
  const validationError = scenario === "command-validation-error" || scenario === "workspace-large-insufficient-space";
  const warning = scenario === "files-publish-warning";
  return {
    schema_version: 1,
    runnable: !validationError,
    is_customized: scenario.includes("customized") || warning || scenario.includes("large") || scenario.includes("remount"),
    plan_hash: `fixture-hash-${scenario}`,
    canonical_plan: plan,
    effective_environment: { test_workspace_root: "/tmp/eos-benchmark-fixture", workspace_root_identity: "fixture-root", client_cohort: "direct_client", image_digest: "sha256:fixture", filesystem: "ext4", free_space_bytes: validationError ? 1_024 : 10_737_418_240, gateway_mode: "isolated" },
    fixed_lifecycle_policy: { lifecycle_revision: 1, failure_revision: 1, stabilization_revision: 1, automatic_retries: 0, one_active_campaign: true, sequential_families: true },
    selected_workspace_profiles: DEFINITIONS_FIXTURE.catalog.workspace_profiles.profiles,
    cells: [],
    execution_blocks: [],
    estimates: { cell_count: plan.operations.length, trial_batch_count: plan.operations.length * 6, issued_operation_request_count: plan.operations.length * 6, duration_range: { minimum_ns: 1_000_000, maximum_ns: 30_000_000 }, estimated_peak_disk_bytes: 16_777_216, required_free_space_bytes: validationError ? 2_147_483_648 : 33_554_432, gateway_restart_count: scenario === "layerstack-remount-restarts" ? 2 : 0, warnings: warning ? ["Publish mutations require fresh isolation."] : [] },
    validation: validationError
      ? [{ severity: "error", code: "insufficient_free_space", message: "The selected fixture exceeds available free space.", path: "operations" }]
      : warning
        ? [{ severity: "warning", code: "publish_isolation", message: "Publish mutations use fresh sandbox isolation.", path: "operations.file_write" }]
        : [],
  };
}

export function healthFixture(scenario: UiFixtureName): HealthResponse {
  const unavailable = scenario === "overview-runner-unavailable";
  return {
    schema_version: 1,
    status: unavailable ? "unready" : "ready",
    execution_ready: !unavailable,
    version: "0.1.0-fixture",
    runner_instance_id: "runner-fixture",
    active_run: null,
    checks: [{ id: "execution_backend", status: unavailable ? "fail" : "pass", message: unavailable ? "Docker product path is unavailable." : "Isolated product path is ready." }],
  };
}

export function settingsFixture(scenario: UiFixtureName): SettingsResponse {
  const unwritable = scenario === "overview-root-unwritable";
  return {
    schema_version: 1,
    test_workspace_root: "/tmp/eos-benchmark-fixture",
    source: "command_line",
    writable: !unwritable,
    path_health: { canonical: true, root_marker: !unwritable, outside_repository: true },
  };
}

const runSummaries: RunListResponse = {
  schema_version: 1,
  runs: [
    { run_id: "run-reference", name: "quick-smoke reference", state: "completed", plan_hash: "hash-reference", configuration_scope: "all", source_commit: "1111111", source_dirty: false, started_at: "2026-07-12T00:00:00Z", ended_at: "2026-07-12T00:01:00Z", correctness: "pass" },
    { run_id: "run-candidate", name: "quick-smoke candidate", state: "completed", plan_hash: "hash-candidate", configuration_scope: "all", source_commit: "2222222", source_dirty: false, started_at: "2026-07-12T01:00:00Z", ended_at: "2026-07-12T01:01:00Z", correctness: "pass" },
  ],
  next_cursor: null,
};

export function runsFixture(): RunListResponse {
  return structuredClone(runSummaries);
}

export function runFixture(scenario: UiFixtureName): RunResponse {
  const cancelling = scenario === "run-cancelling";
  return {
    schema_version: 1,
    manifest: { ...runSummaries.runs[0]!, run_id: "run-fixture", name: "Quick Smoke live evidence", state: cancelling ? "cancelling" : "running", correctness: scenario === "run-correctness-failed" ? "fail" : "pending", ended_at: null, definition_snapshot_version: 2, environment_fingerprint: "fixture-environment" },
    progress: { current_family: "command", current_operation: "exec_command", current_cell_id: "cell-command", current_trial_id: "trial-0004", trial_kind: "measured", phase: scenario === "run-running-setup" ? "setup" : "operation", completed_trial_batches: 3, total_trial_batches: 42, issued_operation_requests: 3, warning_count: scenario === "run-reconnecting" ? 1 : 0, failure_count: scenario === "run-correctness-failed" ? 1 : 0 },
    latest_sequence: 3,
    report_ready: false,
  };
}

export function eventFixtures(scenario: UiFixtureName): EventRecord[] {
  const data: EventData = scenario === "run-correctness-failed"
    ? { kind: "correctness", cell_id: "cell-command", trial_id: "trial-0004", check_id: "command_output", passed: false, expected: "fixture hash", actual: "different hash", artifact_id: "checks" }
    : scenario === "run-running-setup"
      ? { kind: "trial_phase", cell_id: "cell-command", trial_id: "trial-0004", warmup: false, phase: "setup", state: "running" }
      : { kind: "resource_window", cell_id: "cell-command", trial_id: "trial-0004", metric_id: "sandbox_cpu_time", value: scenario === "run-reconnecting" ? null : 5_000_000, unavailable_reason: scenario === "run-reconnecting" ? "counter temporarily unavailable" : null };
  return [
    { sequence: 1, run_id: "run-fixture", monotonic_offset_ns: 2_000_000, data: { kind: "cell_state", cell_id: "cell-command", state: "running" } },
    { sequence: 2, run_id: "run-fixture", monotonic_offset_ns: 3_000_000, data: { kind: "trial_phase", cell_id: "cell-command", trial_id: "trial-0004", warmup: false, phase: "setup", state: "running" } },
    { sequence: 3, run_id: "run-fixture", monotonic_offset_ns: 4_000_000, data: { kind: "request_state", cell_id: "cell-command", trial_id: "trial-0004", request_id: "request-0004", state: "waiting_at_barrier" } },
    { sequence: 4, run_id: "run-fixture", monotonic_offset_ns: 8_000_000, data },
  ];
}

export function statisticsFixture(count: number, unavailable = false): SampleStatistics {
  const empty = count === 0 || unavailable;
  const values = Array.from({ length: Math.min(count, 29) }, (_, index) => 900_000 + index * 10_000);
  return {
    schema_version: 1,
    count,
    minimum: empty ? null : 900_000,
    maximum: empty ? null : 1_200_000,
    mean: empty ? null : 1_050_000,
    sample_standard_deviation: empty ? null : 80_000,
    median: empty ? null : 1_040_000,
    median_absolute_deviation: empty ? null : 40_000,
    p25: empty ? null : 980_000,
    p75: empty ? null : 1_100_000,
    p95: empty ? null : 1_180_000,
    coefficient_of_variation: empty ? null : 0.076,
    median_confidence_interval: count >= 5 && !unavailable ? { level: 0.95, lower: 1_000_000, upper: 1_090_000, method: "percentile_bootstrap_median", resamples: 10_000 } : null,
    confidence_interval_omission: count >= 5 && !unavailable ? null : "insufficient_n",
    p95_exploratory: count < 20,
    outlier_indices: [],
    distribution: empty ? { kind: "empty" } : count < 30 ? { kind: "raw_points", values } : { kind: "histogram_ecdf", histogram: { method: "freedman_diaconis", edges: [900_000, 1_050_000, 1_200_000], counts: [15, 15] }, ecdf: [{ value: 900_000, cumulative_probability: 1 / 30 }, { value: 1_200_000, cumulative_probability: 1 }] },
  };
}

const available = <T,>(value: T): Availability<T> => ({ availability: "available", value });
const unavailableAllocation: Availability<number> = {
  availability: "unavailable",
  source: "filesystem_allocation_probe",
  reason: "allocated-byte counter unavailable for this snapshot",
};

function storageSnapshot(
  monotonicOffsetNs: number,
  manifestVersion: number,
  rootHash: string,
  sampled: boolean,
  allocationsAvailable = true,
): StorageSnapshot {
  return {
    monotonic_offset_ns: available(monotonicOffsetNs),
    sampled,
    manifest_version: available(manifestVersion),
    root_hash: available(rootHash),
    active_layer_count: available(manifestVersion === 1 ? 8 : 4),
    active_lease_count: available(4),
    active_logical_bytes: available(manifestVersion === 1 ? 32_768 : 16_384),
    active_allocated_bytes: allocationsAvailable ? available(manifestVersion === 1 ? 40_960 : 20_480) : unavailableAllocation,
    storage_logical_bytes: available(manifestVersion === 1 ? 65_536 : 49_152),
    storage_allocated_bytes: allocationsAvailable ? available(manifestVersion === 1 ? 81_920 : 61_440) : unavailableAllocation,
    staging_entry_count: available(manifestVersion === 1 && sampled ? 2 : 0),
  };
}

export const COMMAND_OPERATION_EVIDENCE_FIXTURE = {
  trial_id: "trial-measured-0001",
  request_id: "request-command-0001",
  evidence: {
    operation: "exec_command",
    evidence: {
      command_case: "noop",
      template_revision: 1,
      command_sha256: "command-sha256",
      exit_code: 0,
      stdout: { byte_count: 0, truncated: false, sha256: "stdout-sha256" },
      stderr: { byte_count: 0, truncated: false, sha256: "stderr-sha256" },
    },
  },
} satisfies OperationEvidenceReport;

export const LAYERSTACK_OPERATION_EVIDENCE_FIXTURE = {
  trial_id: "trial-layerstack-measured-0001",
  request_id: "request-layerstack-0001",
  evidence: {
    operation: "squash_layerstack",
    evidence: {
      requested_live_sessions: 4,
      observed_migrated_sessions: 2,
      observed_non_migrated_sessions: 2,
      dispositions: { migrated: 2, identity: 1, leased: 1, faulty: 0, session_gone: 0 },
      effective_remount_parallelism: 2,
      observed_squashed_block_count: 1,
      observed_replaced_layer_count: 4,
      source_layer_ids: ["layer-source-a", "layer-source-b"],
      retained_source_layer_ids: ["layer-source-b"],
      source_layer_allocations: [
        { layer_id: "layer-source-a", logical_bytes: available(8_192), allocated_bytes: available(12_288) },
        { layer_id: "layer-source-b", logical_bytes: available(8_192), allocated_bytes: unavailableAllocation },
      ],
      reclaimed_bytes: available(20_480),
      s0_baseline: storageSnapshot(0, 1, "root-s0", false),
      s1_sampled_peak: storageSnapshot(5_000_000, 1, "root-s1", true, false),
      s2_post_commit: storageSnapshot(8_000_000, 2, "root-s2", false),
      s3_settled: storageSnapshot(12_000_000, 2, "root-s3", false),
      manifest_reduced: true,
      content_equivalent: true,
      usable_session_count: 4,
    },
  },
} satisfies OperationEvidenceReport;

export const LAYERSTACK_OPERATION_OBSERVATION_FIXTURE: OperationObservationRecord = {
  record: "operation",
  data: {
    operation_id: "squash_layerstack",
    cell_id: "cell-layerstack",
    trial_id: LAYERSTACK_OPERATION_EVIDENCE_FIXTURE.trial_id,
    request_id: LAYERSTACK_OPERATION_EVIDENCE_FIXTURE.request_id,
    evidence: LAYERSTACK_OPERATION_EVIDENCE_FIXTURE.evidence,
  },
};

export function reportFixture(scenario: UiFixtureName): ReportResponse {
  type Cell = ReportResponse["cells"][number];
  type ReportMetric = Cell["metrics"][number];
  type ReportFactor = Cell["factors"][number];
  type ReportFactorValue = ReportFactor["value"];

  const measuredTrials = scenario === "report-complete-n30" ? 30 : 29;
  const resourceUnavailable = scenario === "report-partial-unavailable-resource";
  const correlationAvailable = scenario === "report-cpu-latency-correlation";
  const layerstackIncluded = scenario === "report-complete-n30";
  const commandTrialBatches = measuredTrials + 1;

  const timingSpecs: {
    id: string;
    label: string;
    help: string;
    unit: ReportMetric["identity"]["unit"];
    direction: ReportMetric["identity"]["direction"];
    source: string;
    base: number;
    requestScoped: boolean;
    integerBacked: boolean;
  }[] = [
    { id: "batch_makespan_ns", label: "Batch makespan", help: "Barrier release until the last issued product request reaches a terminal response.", unit: "nanoseconds", direction: "lower_is_preferred", source: "runner_monotonic_batch_barrier", base: 960_000, requestScoped: false, integerBacked: true },
    { id: "request_latency_ns", label: "Request latency", help: "One issued product request from send until its final response is decoded.", unit: "nanoseconds", direction: "lower_is_preferred", source: "runner_monotonic_product_request", base: 210_000, requestScoped: true, integerBacked: true },
    { id: "throughput_ops_s", label: "Throughput", help: "Successful issued product requests divided by batch makespan seconds.", unit: "operations_per_second", direction: "higher_is_preferred", source: "successful_requests_per_batch_makespan", base: 3_800, requestScoped: false, integerBacked: false },
    { id: "setup_ns", label: "Setup", help: "Harness setup time outside the primary operation window.", unit: "nanoseconds", direction: "descriptive_only", source: "runner_monotonic_lifecycle", base: 420_000, requestScoped: false, integerBacked: true },
    { id: "verify_ns", label: "Verification", help: "Correctness verification time outside the primary operation window.", unit: "nanoseconds", direction: "descriptive_only", source: "runner_monotonic_lifecycle", base: 180_000, requestScoped: false, integerBacked: true },
    { id: "teardown_ns", label: "Teardown", help: "Owned cleanup and baseline verification time outside the primary operation window.", unit: "nanoseconds", direction: "descriptive_only", source: "runner_monotonic_lifecycle", base: 310_000, requestScoped: false, integerBacked: true },
  ];

  const rawPoints = (
    prefix: string,
    trialCount: number,
    requestsPerTrial: number,
    base: number,
    integerBacked: boolean,
  ): ReportMetric["raw_points"] => {
    const pointsPerTrial = requestsPerTrial === 0 ? 1 : requestsPerTrial;
    return Array.from({ length: trialCount * pointsPerTrial }, (_, index) => {
      const trialIndex = Math.floor(index / pointsPerTrial) + 1;
      const requestIndex = index % pointsPerTrial + 1;
      const value = base + trialIndex * 3_100 + (requestsPerTrial === 0 ? 0 : requestIndex * 700);
      return {
        trial_id: `trial-${prefix}-${String(trialIndex).padStart(4, "0")}`,
        request_id: requestsPerTrial === 0 ? null : `request-${prefix}-${String(trialIndex).padStart(4, "0")}-${String(requestIndex).padStart(2, "0")}`,
        value,
        raw_integer_value: integerBacked ? Math.round(value) : null,
        outlier: index === trialCount * pointsPerTrial - 1,
      };
    });
  };

  const cpuIdentity: ReportMetric["identity"] = {
    id: "sandbox_cpu_time_ns",
    label: "Sandbox CPU time",
    help: "Sandbox cumulative cgroup CPU-use counter delta over the trial window.",
    semantic_revision: 1,
    unit: "nanoseconds",
    scope: "sandbox",
    kind: "monotonic_counter",
    availability: "explicit_unavailable",
    aggregation: "delta",
    direction: "lower_is_preferred",
    source: "sandbox_resource_counter",
    ratio_scale: false,
    report_derivation_revision: 3,
  };

  const metricSet = (
    prefix: string,
    trialCount: number,
    requestsPerTrial: number,
    unavailableCpu: boolean,
  ): Cell["metrics"] => {
    const timingMetrics = timingSpecs.map((spec): ReportMetric => {
      const points = rawPoints(
        prefix,
        trialCount,
        spec.requestScoped ? requestsPerTrial : 0,
        spec.base,
        spec.integerBacked,
      );
      return {
        identity: {
          id: spec.id,
          label: spec.label,
          help: spec.help,
          semantic_revision: 1,
          unit: spec.unit,
          scope: "operation",
          kind: "gauge",
          availability: "explicit_unavailable",
          aggregation: "mean",
          direction: spec.direction,
          source: spec.source,
          ratio_scale: true,
          report_derivation_revision: 3,
        },
        attempted_n: points.length,
        failed_n: 0,
        available_n: points.length,
        unavailable: { count: 0, reasons: {} },
        statistics: statisticsFixture(points.length),
        raw_points: points,
      };
    });
    const cpuPoints = unavailableCpu ? [] : rawPoints(`${prefix}-cpu`, trialCount, 0, 700_000, true);
    return [...timingMetrics, {
      identity: structuredClone(cpuIdentity),
      attempted_n: trialCount,
      failed_n: 0,
      available_n: cpuPoints.length,
      unavailable: {
        count: unavailableCpu ? trialCount : 0,
        reasons: unavailableCpu ? { "counter unavailable": trialCount } : {},
      },
      statistics: statisticsFixture(cpuPoints.length, unavailableCpu),
      raw_points: cpuPoints,
    }];
  };

  const commandFactors: Cell["factors"] = [
    { id: "concurrent_requests", label: "Concurrent requests", help: "Product requests issued at the same time within this test combination.", role: "varied", unit: "count", value: { kind: "unsigned_integer", value: 4 }, control: { kind: "unsigned_integer", value: 1 } },
    { id: "workspace_profile", label: "Workspace profile", help: "Deterministic fixture size used for every command trial.", role: "controlled", unit: null, value: { kind: "choice", value: "small" }, control: null },
    { id: "session_mode", label: "Session mode", help: "Workspace-session isolation applied to command requests.", role: "controlled", unit: null, value: { kind: "choice", value: "explicit" }, control: null },
    { id: "command_case", label: "Command case", help: "Allowlisted bounded-shell command template.", role: "controlled", unit: null, value: { kind: "choice", value: "noop" }, control: null },
  ];
  const commandControlFactors: Cell["factors"] = commandFactors.map((factor) => factor.id === "concurrent_requests"
    ? { ...factor, value: { kind: "unsigned_integer", value: 1 } }
    : structuredClone(factor));

  const layerstackFactors: Cell["factors"] = [
    { id: "live_sessions", label: "Live sessions", help: "Prepared live-session load held during the single squash request.", role: "varied", unit: "count", value: { kind: "unsigned_integer", value: 4 }, control: { kind: "unsigned_integer", value: 0 } },
    { id: "remount_parallelism", label: "Remount parallelism", help: "Bound on simultaneous session remount work after commit.", role: "varied", unit: "count", value: { kind: "unsigned_integer", value: 2 }, control: { kind: "unsigned_integer", value: 1 } },
    { id: "requested_migration_ratio", label: "Requested migration ratio", help: "Requested share of prepared sessions that should migrate.", role: "controlled", unit: "ratio", value: { kind: "ratio", value: 0.5 }, control: null },
    { id: "squashable_blocks", label: "Squashable blocks", help: "Prepared layer blocks eligible for the request.", role: "controlled", unit: "count", value: { kind: "unsigned_integer", value: 1 }, control: null },
    { id: "layers_per_block", label: "Layers per block", help: "Prepared source layers in each squashable block.", role: "controlled", unit: "count", value: { kind: "unsigned_integer", value: 4 }, control: null },
    { id: "payload_bytes", label: "Payload bytes", help: "Deterministic payload written into every source layer.", role: "controlled", unit: "bytes", value: { kind: "unsigned_integer", value: 16_384 }, control: null },
    { id: "session_activity", label: "Session activity", help: "Prepared session workload while the squash request runs.", role: "controlled", unit: null, value: { kind: "choice", value: "idle" }, control: null },
  ];
  const withFactorValue = (factors: Cell["factors"], id: ReportFactor["id"], value: ReportFactorValue): Cell["factors"] => factors.map((factor) => factor.id === id ? { ...factor, value } : structuredClone(factor));
  const layerstackControlFactors = withFactorValue(
    withFactorValue(layerstackFactors, "live_sessions", { kind: "unsigned_integer", value: 0 }),
    "remount_parallelism",
    { kind: "unsigned_integer", value: 1 },
  );

  const emptyCorrelation = (unavailableCpu = 0): Cell["cpu_latency_correlation"] => ({
    semantic_revision: 1,
    method: "pearson",
    alignment: "eligible_trial_aggregate_by_trial_id",
    eligibility: "measured_product_success_checks_pass_cleanup_restored",
    latency_metric_id: "batch_makespan_ns",
    cpu_metric_id: "sandbox_cpu_time_ns",
    support_count: 0,
    coefficient: null,
    confidence_interval: null,
    interval_omission: "insufficient_n",
    points: [],
    exclusions: { ineligible_trial: 0, missing_latency: 0, missing_cpu: 0, unavailable_cpu: unavailableCpu },
  });
  const correlation: Cell["cpu_latency_correlation"] = correlationAvailable ? {
    semantic_revision: 1,
    method: "pearson",
    alignment: "eligible_trial_aggregate_by_trial_id",
    eligibility: "measured_product_success_checks_pass_cleanup_restored",
    latency_metric_id: "batch_makespan_ns",
    cpu_metric_id: "sandbox_cpu_time_ns",
    support_count: 5,
    coefficient: 0.75,
    confidence_interval: { level: 0.95, lower: 0.31, upper: 0.92, method: "percentile_bootstrap_pearson", resamples: 10_000, valid_resamples: 9_978 },
    interval_omission: null,
    points: Array.from({ length: 5 }, (_, index) => ({
      trial_id: `trial-correlation-${index + 1}`,
      operation_latency_ns: 1_000_000 + index * 45_000,
      sandbox_cpu_time_ns: 700_000 + index * 31_000,
    })),
    exclusions: { ineligible_trial: 0, missing_latency: 0, missing_cpu: 0, unavailable_cpu: 0 },
  } : emptyCorrelation(resourceUnavailable ? measuredTrials : 0);

  const commandMetrics = metricSet("command", measuredTrials, 4, resourceUnavailable);
  const commandControlMetrics = metricSet("command-control", measuredTrials, 1, resourceUnavailable);
  const commandTimelines: Cell["timelines"] = [{
    trial_id: "trial-measured-0001",
    domain_start_ns: 0,
    domain_end_ns: 12_000_000,
    operation_window: { start_offset_ns: 2_000_000, duration_ns: 5_000_000 },
    request_spans: [{ request_id: "request-command-0001", start_offset_ns: 2_000_000, duration_ns: 5_000_000, succeeded: true, status: "succeeded" }],
    phase_spans: [],
    series: [{
      identity: structuredClone(cpuIdentity),
      request_id: "request-command-0001",
      points: [
        { monotonic_offset_ns: 1_000_000, sampled: true, value: resourceUnavailable ? { availability: "unavailable", source: "sandbox_resource_counter", reason: "counter unavailable" } : available(700_000) },
        { monotonic_offset_ns: 6_000_000, sampled: true, value: resourceUnavailable ? { availability: "unavailable", source: "sandbox_resource_counter", reason: "counter unavailable" } : available(760_000) },
        { monotonic_offset_ns: 10_000_000, sampled: false, value: { availability: "unavailable", source: "sandbox_resource_counter", reason: "sample outside the exact request window" } },
      ],
    }],
  }];

  const commandCell: Cell = {
    cell_id: "cell-command",
    family_id: "command",
    family_label: scenario === "report-old-definition-snapshot" ? "Historical Command Label" : "Command",
    operation_id: "exec_command",
    operation_label: "Execute command",
    comparison_key: { operation: "exec_command", semantic_revision: 1, concurrent_requests: 4 },
    design_counts: { test_combinations: 1, trial_batches: commandTrialBatches, issued_product_requests: commandTrialBatches * 4 },
    factors: commandFactors,
    counts: { total_attempted: commandTrialBatches, warmup: 1, measured_attempted: measuredTrials, successful: measuredTrials, product_failed: 0, correctness_failed: 0, infrastructure_failed: 0, cleanup_invalid: 0, missing_primary_latency: 0 },
    metrics: commandMetrics,
    checks: [{ id: "command_output", label: "Command output", help: "Exit output must match the bounded allowlisted command template.", semantic_revision: 1, attempted: measuredTrials, passed: measuredTrials, failed: 0 }],
    phases: [],
    timelines: commandTimelines,
    check_evidence: [{
      id: "command_output",
      label: "Command output",
      help: "Exit output must match the bounded allowlisted command template.",
      semantic_revision: 1,
      trial_id: "trial-measured-0001",
      request_id: "request-command-0001",
      verdict: "pass",
      duration_ns: 125_000,
      evidence: {
        items: [
          { expected: "exit code 0", actual: "exit code 0", artifact_id: null },
          { expected: "stdout SHA-256 stdout-sha256", actual: "stdout SHA-256 stdout-sha256", artifact_id: "checks/command-output.json" },
        ],
        truncated_count: 2,
        truncated_sha256: "f7a44f3e6c6ab02a33d887e4746bb00f105e0415ce54a362e46e5a365f0c06fd",
      },
    }],
    operation_evidence: [structuredClone(COMMAND_OPERATION_EVIDENCE_FIXTURE)],
    cpu_latency_correlation: correlation,
  };
  const commandControlCell: Cell = {
    ...structuredClone(commandCell),
    cell_id: "cell-command-control",
    comparison_key: { operation: "exec_command", semantic_revision: 1, concurrent_requests: 1 },
    design_counts: { test_combinations: 1, trial_batches: commandTrialBatches, issued_product_requests: commandTrialBatches },
    factors: commandControlFactors,
    metrics: commandControlMetrics,
    timelines: [],
    check_evidence: [],
    operation_evidence: [],
    cpu_latency_correlation: emptyCorrelation(resourceUnavailable ? measuredTrials : 0),
  };

  const layerstackMetrics = metricSet("layerstack", 1, 1, false);
  const layerstackControlMetrics = metricSet("layerstack-control", 1, 1, false);
  const layerstackTimelines: Cell["timelines"] = [{
    trial_id: "trial-layerstack-measured-0001",
    domain_start_ns: 0,
    domain_end_ns: 12_000_000,
    operation_window: { start_offset_ns: 1_000_000, duration_ns: 9_000_000 },
    request_spans: [{ request_id: "request-layerstack-0001", start_offset_ns: 1_000_000, duration_ns: 9_000_000, succeeded: true, status: "succeeded" }],
    phase_spans: LAYERSTACK_PHASES.map((phase, index) => ({
      id: phase.id,
      label: phase.label,
      help: phase.help,
      semantic_revision: phase.semantic_revision,
      request_id: "request-layerstack-0001",
      start_offset_ns: [1_000_000, 1_500_000, 2_500_000, 5_000_000, 6_000_000, 6_500_000][index] ?? 1_000_000,
      duration_ns: [9_000_000, 1_000_000, 2_500_000, 1_000_000, 3_000_000, 1_000_000][index] ?? 1_000_000,
      status: "succeeded",
    })),
    series: [{
      identity: {
        id: "sandbox_memory_current_bytes",
        label: "Sandbox memory current",
        help: "Maximum sampled sandbox cgroup memory.current bytes.",
        semantic_revision: 1,
        unit: "bytes",
        scope: "sandbox",
        kind: "gauge",
        availability: "explicit_unavailable",
        aggregation: "maximum",
        direction: "lower_is_preferred",
        source: "sandbox_resource_counter",
        ratio_scale: true,
        report_derivation_revision: 3,
      },
      request_id: "request-layerstack-0001",
      points: [
        { monotonic_offset_ns: 500_000, sampled: true, value: available(65_536) },
        { monotonic_offset_ns: 5_500_000, sampled: true, value: available(98_304) },
        { monotonic_offset_ns: 9_500_000, sampled: true, value: { availability: "unavailable", source: "cgroup_v2", reason: "memory.current disappeared during teardown" } },
      ],
    }],
  }];
  const layerstackChecks: Cell["checks"] = [
    { id: "layerstack_content_equivalence", label: "Content equivalence", help: "Settled content must match the pre-squash logical view.", semantic_revision: 1, attempted: 1, passed: 1, failed: 0 },
    { id: "layerstack_manifest_reduction", label: "Manifest reduction", help: "The committed manifest must replace the prepared source layers.", semantic_revision: 1, attempted: 1, passed: 1, failed: 0 },
    { id: "layerstack_disposition_accounting", label: "Disposition accounting", help: "Every prepared live session must have exactly one disposition.", semantic_revision: 1, attempted: 1, passed: 1, failed: 0 },
    { id: "layerstack_session_usability", label: "Session usability", help: "Prepared sessions must remain usable after the remount sweep.", semantic_revision: 1, attempted: 1, passed: 1, failed: 0 },
    { id: "layerstack_residue", label: "LayerStack residue", help: "Cleanup must restore the owned fixture baseline.", semantic_revision: 1, attempted: 1, passed: 1, failed: 0 },
  ];
  const layerstackCell: Cell = {
    cell_id: "cell-layerstack",
    family_id: "layer_stack",
    family_label: "LayerStack",
    operation_id: "squash_layerstack",
    operation_label: "Squash LayerStack",
    comparison_key: { operation: "squash_layerstack", semantic_revision: 1, live_sessions: 4, remount_parallelism: 2 },
    design_counts: { test_combinations: 1, trial_batches: 2, issued_product_requests: 2 },
    factors: layerstackFactors,
    counts: { total_attempted: 2, warmup: 1, measured_attempted: 1, successful: 1, product_failed: 0, correctness_failed: 0, infrastructure_failed: 0, cleanup_invalid: 0, missing_primary_latency: 0 },
    metrics: layerstackMetrics,
    checks: layerstackChecks,
    phases: LAYERSTACK_PHASES.map((phase) => ({ ...phase, attempted: 1, failed: 0, duration: statisticsFixture(1) })),
    timelines: layerstackTimelines,
    check_evidence: [{
      id: "layerstack_content_equivalence",
      label: "Content equivalence",
      help: "Settled content must match the pre-squash logical view.",
      semantic_revision: 1,
      trial_id: "trial-layerstack-measured-0001",
      request_id: "request-layerstack-0001",
      verdict: "pass",
      duration_ns: 180_000,
      evidence: {
        items: [{ expected: "root hash root-s0", actual: "root hash root-s3 with equivalent logical content", artifact_id: "checks/layerstack-content-equivalence.json" }],
        truncated_count: 0,
        truncated_sha256: null,
      },
    }],
    operation_evidence: [structuredClone(LAYERSTACK_OPERATION_EVIDENCE_FIXTURE)],
    cpu_latency_correlation: emptyCorrelation(),
  };
  const layerstackControlCell: Cell = {
    ...structuredClone(layerstackCell),
    cell_id: "cell-layer-control",
    comparison_key: { operation: "squash_layerstack", semantic_revision: 1, live_sessions: 0, remount_parallelism: 1 },
    factors: layerstackControlFactors,
    metrics: layerstackControlMetrics,
    timelines: [],
    check_evidence: [],
    operation_evidence: [],
  };

  const cells: ReportResponse["cells"] = [commandCell, commandControlCell];
  if (layerstackIncluded) cells.push(layerstackCell, layerstackControlCell);

  const primaryCommandMetric = commandMetrics[0]!;
  const primaryCommandControlMetric = commandControlMetrics[0]!;
  const factorStudies: ReportResponse["factor_studies"] = [{
    operation_id: "exec_command",
    operation_label: "Execute command",
    metric: structuredClone(primaryCommandMetric.identity),
    layout: { kind: "trend", factor_id: "concurrent_requests" },
    varied_factor_ids: ["concurrent_requests"],
    controlled_factor_ids: ["workspace_profile", "session_mode", "command_case"],
    cells: [
      { cell_id: "cell-command-control", factors: commandControlFactors, successful_n: measuredTrials, failed_n: 0, median: primaryCommandControlMetric.statistics.median, confidence_interval: primaryCommandControlMetric.statistics.median_confidence_interval, interval_omission_reason: primaryCommandControlMetric.statistics.confidence_interval_omission, raw_points: primaryCommandControlMetric.raw_points },
      { cell_id: "cell-command", factors: commandFactors, successful_n: measuredTrials, failed_n: 0, median: primaryCommandMetric.statistics.median, confidence_interval: primaryCommandMetric.statistics.median_confidence_interval, interval_omission_reason: primaryCommandMetric.statistics.confidence_interval_omission, raw_points: primaryCommandMetric.raw_points },
    ],
    control_comparisons: [{
      comparison_id: "exec-command-batch-makespan-control",
      control_cell_id: "cell-command-control",
      candidate_cell_id: "cell-command",
      changed_factor_ids: ["concurrent_requests"],
      control_median: primaryCommandControlMetric.statistics.median,
      candidate_median: primaryCommandMetric.statistics.median,
      absolute_difference: 0,
      percentage_difference: 0,
      median_difference_confidence_interval: { level: 0.95, lower: -50_000, upper: 50_000, method: "percentile_bootstrap_median_difference", resamples: 10_000 },
      interval_omission_reason: null,
    }],
  }];
  if (layerstackIncluded) {
    const layerstackPrimary = layerstackMetrics[0]!;
    const layerstackControlPrimary = layerstackControlMetrics[0]!;
    factorStudies.push({
      operation_id: "squash_layerstack",
      operation_label: "Squash LayerStack",
      metric: structuredClone(layerstackPrimary.identity),
      layout: { kind: "matrix", row_factor_id: "live_sessions", column_factor_id: "remount_parallelism" },
      varied_factor_ids: ["live_sessions", "remount_parallelism"],
      controlled_factor_ids: ["requested_migration_ratio", "squashable_blocks", "layers_per_block", "payload_bytes", "session_activity"],
      cells: [
        { cell_id: "cell-layer-control", factors: layerstackControlFactors, successful_n: 1, failed_n: 0, median: layerstackControlPrimary.statistics.median, confidence_interval: null, interval_omission_reason: "insufficient_n", raw_points: layerstackControlPrimary.raw_points },
        { cell_id: "cell-layerstack", factors: layerstackFactors, successful_n: 1, failed_n: 0, median: layerstackPrimary.statistics.median, confidence_interval: null, interval_omission_reason: "insufficient_n", raw_points: layerstackPrimary.raw_points },
      ],
      control_comparisons: [{
        comparison_id: "layerstack-batch-makespan-control",
        control_cell_id: "cell-layer-control",
        candidate_cell_id: "cell-layerstack",
        changed_factor_ids: ["live_sessions", "remount_parallelism"],
        control_median: layerstackControlPrimary.statistics.median,
        candidate_median: layerstackPrimary.statistics.median,
        absolute_difference: 0,
        percentage_difference: 0,
        median_difference_confidence_interval: null,
        interval_omission_reason: "insufficient_n",
      }],
    });
  }

  const reportDesignCounts = {
    test_combinations: cells.length,
    trial_batches: commandTrialBatches * 2 + (layerstackIncluded ? 4 : 0),
    issued_product_requests: commandTrialBatches * 5 + (layerstackIncluded ? 4 : 0),
  };
  const schemaIdentity = (schemaName: string, writeVersion: number, readVersions = [writeVersion]): ReportResponse["methods"]["artifact_schemas"]["run_manifest"] => ({
    schema_name: schemaName,
    write_version: writeVersion,
    read_versions: readVersions,
  });
  const operationAuthorities: ReportResponse["methods"]["operation_authorities"] = [{
    operation_id: "exec_command",
    family_id: "command",
    semantic_revision: 1,
    factor_schema_revision: 1,
    comparison_projection_revision: 1,
    client_cohort: "direct_client",
    product_access: { kind: "public_gateway", action: "exec_command" },
    count_semantics: { kind: "concurrent_requests", factor: "concurrent_requests" },
    cleanup_policy: "verify_fixture_unchanged",
    resolved_isolation_policies: ["fresh_sessions_per_trial"],
    request_timeout_ms: [30_000],
    stabilization_policy: { kind: "not_required", semantic_revision: 1 },
  }];
  if (layerstackIncluded) operationAuthorities.push({
    operation_id: "squash_layerstack",
    family_id: "layer_stack",
    semantic_revision: 1,
    factor_schema_revision: 1,
    comparison_projection_revision: 1,
    client_cohort: "direct_client",
    product_access: { kind: "public_gateway", action: "squash_layerstacks" },
    count_semantics: { kind: "single_request_with_prepared_load", load_factor: "live_sessions" },
    cleanup_policy: "destroy_topology_and_verify_baseline",
    resolved_isolation_policies: ["fresh_topology_per_trial"],
    request_timeout_ms: [120_000],
    stabilization_policy: { kind: "exact_snapshot_quiet_window", semantic_revision: 1, quiet_window_matches: 3, poll_interval_ms: 50, timeout_ms: 5_000 },
  });

  const summary: ReportResponse["summary"] = commandMetrics.slice(0, 6).map((metric) => ({
    row_id: `summary-cell-command-${metric.identity.id}`,
    operation_id: "exec_command",
    cell_id: "cell-command",
    metric_id: metric.identity.id,
    unit: metric.identity.unit,
    successful_n: metric.available_n,
    failed_n: metric.failed_n,
    unavailable_n: metric.unavailable.count,
    median: metric.statistics.median,
    confidence_interval: metric.statistics.median_confidence_interval,
    interval_omission_reason: metric.statistics.confidence_interval_omission,
    direction: metric.identity.direction,
  }));

  return {
    schema_version: 4,
    report_derivation_revision: 3,
    run_id: "run-reference",
    state: "completed",
    provisional: resourceUnavailable,
    correctness_verdict: "pass",
    design_counts: reportDesignCounts,
    research_question: "How does command work respond to load?",
    plan_hash: "fixture-plan-hash",
    source_commit: "1111111",
    source_dirty: false,
    environment_fingerprint: "fixture-environment",
    definition_snapshot_version: scenario === "report-old-definition-snapshot" ? 1 : 2,
    definition_snapshot_sha256: scenario === "report-old-definition-snapshot" ? "old-snapshot-sha256" : "snapshot-sha256",
    started_at: "2026-07-12T00:00:00Z",
    ended_at: "2026-07-12T00:01:00Z",
    summary,
    factor_studies: factorStudies,
    cells,
    methods: {
      schema_version: 1,
      report_derivation_revision: 3,
      artifact_reader_revision: 1,
      plan_schema_version: 1,
      plan_seed: 20_260_712,
      cell_order: "randomized_blocks",
      resource_sample_interval_ms: 50,
      design_counts: structuredClone(reportDesignCounts),
      fixture_generator_revision: 1,
      fixture_hashes: { "workspace-profile-small-v1": "fixture-small-sha256", "layerstack-topology-v1": "fixture-layerstack-sha256" },
      producer: { package: "sandbox-benchmark", version: "0.1.0" },
      artifact_schemas: {
        run_manifest: schemaIdentity("eos_benchmark_run_manifest", 1),
        intent_plan: schemaIdentity("eos_benchmark_intent_plan", 1),
        expanded_plan: schemaIdentity("eos_benchmark_expanded_plan", 1),
        definition_snapshot: schemaIdentity("eos_benchmark_definition_snapshot", scenario === "report-old-definition-snapshot" ? 1 : 2),
        environment_metadata: schemaIdentity("eos_benchmark_environment_metadata", 1),
        events: schemaIdentity("eos_benchmark_event", 1),
        observations: schemaIdentity("eos_benchmark_observation", 3, [1, 2, 3]),
        bounded_evidence: schemaIdentity("eos_benchmark_operation_evidence", 1),
      },
      operation_authorities: operationAuthorities,
      metric_revisions: [
        { metric_id: "sandbox_cpu_time_ns", semantic_revision: 1 },
        ...(layerstackIncluded ? [{ metric_id: "sandbox_memory_current_bytes", semantic_revision: 1 }] : []),
      ],
      derived_metric_revisions: timingSpecs.map(({ id }) => ({ metric_id: id, semantic_revision: 1 })),
      check_revisions: [
        { check_id: "command_output", semantic_revision: 1 },
        ...(layerstackIncluded ? layerstackChecks.map(({ id }) => ({ check_id: id, semantic_revision: 1 })) : []),
      ],
      phase_revisions: layerstackIncluded ? LAYERSTACK_PHASES.map(({ id }) => ({ phase_id: id, semantic_revision: 1 })) : [],
      environment: {
        schema_version: 1,
        treatment: { source_commit: "1111111", source_dirty: false, source_diff_hash: null, daemon_binary_hash: "daemon-binary-sha256", gateway_binary_hash: "gateway-binary-sha256" },
        host: { operating_system: "linux", architecture: "x86_64", kernel_release: "6.8.0-fixture", docker_engine_version: "27.1.1", filesystem: "ext4", monotonic_clock: "std::time::Instant" },
        image_reference: "ephemeralos/benchmark-fixture:2026-07-12",
        image_digest: "sha256:image-fixture",
        workspace_root_identity: "fixture-root",
        client_cohort: "direct_client",
        gateway_endpoint_identity: "isolated_loopback_per_execution_block",
      },
      raw_time_unit: "integer_nanoseconds",
      monotonic_clock: "std::time::Instant",
      quantile_interpolation: "linear_type_7_v1",
      confidence_interval: "deterministic_percentile_bootstrap_median_95_percent",
      bootstrap_resamples: 10_000,
      outlier_policy: "tukey_1_5_iqr_flagged_and_retained",
      warmup_policy: "recorded_but_excluded_from_statistics",
      failure_policy: "only measured product-success, required-check-pass, cleanup-restored trials are eligible",
      resource_policy: "availability is explicit; unavailable samples are never converted to zero",
      comparison_policy: "compatibility_before_delta; independent bootstrap; no p-value or v1 regression verdict",
    },
    limitations: resourceUnavailable ? ["Sandbox CPU was unavailable for every measured trial."] : [],
    warnings: scenario === "report-old-definition-snapshot" ? [{ code: "historical_snapshot", message: "Labels and units come from the immutable run snapshot." }] : [],
  };
}

export function comparisonFixture(scenario: UiFixtureName, descriptiveOverride = false): ComparisonResponse {
  const incompatible = scenario !== "compare-compatible";
  const descriptive = scenario === "compare-descriptive-override" || descriptiveOverride;
  return {
    schema_version: 1,
    comparison_derivation_revision: 2,
    reference_run_id: "run-reference",
    candidate_run_id: "run-candidate",
    protocol: { reference: { protocol_id: "default", protocol_version: 1, treatment_fields: [], source: "defaulted" }, candidate: { protocol_id: incompatible ? "candidate" : "default", protocol_version: 1, treatment_fields: [], source: "defaulted" }, declarations_compatible: !incompatible },
    compatible: !incompatible,
    descriptive_only: descriptive,
    treatment_differences: incompatible ? ["Protocol id differs."] : [],
    typed_treatment_differences: [],
    checks: [{ check_id: "protocol_declaration", label: "Protocol declaration", compatible: !incompatible, consequence: incompatible ? "Aggregate deltas are blocked." : "Cells may be matched.", scope: "core_invariant", blocks_aggregate: true }],
    matched_cell_ids: descriptive || !incompatible ? ["cell-command"] : [],
    matched_cells: descriptive || !incompatible ? [{ match_id: "match-command", comparison_key_sha256: "key-sha256", operation_id: "exec_command", reference_cell_id: "cell-command", candidate_cell_id: "cell-command", effective_protocol_compatible: !incompatible }] : [],
    deltas: descriptive || !incompatible ? [{ comparison_id: "delta-batch-makespan", match_id: "match-command", reference_cell_id: "cell-command", candidate_cell_id: "cell-command", metric_id: "batch_makespan_ns", unit: "nanoseconds", reference_unit: "nanoseconds", candidate_unit: "nanoseconds", reference_value: 1_000_000, candidate_value: 1_100_000, reference_n: 30, candidate_n: 30, reference_unavailable_n: 0, candidate_unavailable_n: 0, absolute_change: 100_000, percent_change: 10, median_difference_confidence_interval: { level: 0.95, lower: 50_000, upper: 150_000, method: "percentile_bootstrap_median", resamples: 10_000 }, confidence_interval_omission_reason: null, unavailable_reason: null, direction: "lower_is_preferred", descriptive_only: descriptive, correctness: { reference_correctness_failed: 0, candidate_correctness_failed: 0, reference_cleanup_invalid: 0, candidate_cleanup_invalid: 0 } }] : [],
    performance_verdict: !incompatible ? "candidate slower on batch makespan" : null,
  };
}

export function terminalStateForScenario(scenario: UiFixtureName): RunState {
  return scenario === "run-cancelling" ? "cancelling" : "running";
}
