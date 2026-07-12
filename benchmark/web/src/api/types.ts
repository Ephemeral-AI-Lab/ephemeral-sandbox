/** Canonical API family identifiers used by definitions, cells, and reports. */
export type FamilyId = "command" | "files" | "workspace_lifecycle" | "layer_stack";

/**
 * Persisted `family_state` event names. This is intentionally separate from
 * `FamilyId`: the LayerStack event namespace is `layerstack`, while its typed
 * definition/cell/report identity is `layer_stack`.
 */
export type FamilyEventName = "command" | "files" | "workspace_lifecycle" | "layerstack";

export type OperationId =
  | "exec_command"
  | "file_read"
  | "file_write"
  | "file_edit"
  | "file_blame"
  | "create_workspace"
  | "squash_layerstack";

export type ConfigurationScope = "all" | "command" | "files" | "workspace" | "layerstack";
export type ClientCohort = "direct_client" | "cli_e2e";
export type FactorRole = "varied" | "controlled";
export type WorkspaceProfileId = string;
export type AllowedNetworkProfile = "shared" | "isolated";
export type ResolvedIsolationPolicy =
  | "reusable_verified_fixture"
  | "fresh_sessions_per_trial"
  | "fresh_sandbox_per_trial"
  | "prepared_sandbox_per_cell"
  | "fresh_topology_per_trial";
export type CleanupPolicy =
  | "verify_fixture_unchanged"
  | "resolve_from_isolation"
  | "destroy_sessions_and_verify_baseline"
  | "destroy_topology_and_verify_baseline";

export interface Factor<T> {
  role: FactorRole;
  values: T[];
  control: T | null;
}

export interface ExecCommandFactors {
  concurrent_requests: Factor<number>;
  workspace_profile: Factor<WorkspaceProfileId>;
  session_mode: Factor<"explicit" | "automatic">;
  command_case: Factor<"noop" | "output64_kib" | "cpu50_ms" | "fixture_read">;
}

export interface FileReadFactors {
  concurrent_requests: Factor<number>;
  returned_bytes: Factor<number>;
  source: Factor<"snapshot" | "session">;
  target_mode: Factor<"independent" | "same_target">;
}

export interface FileWriteFactors {
  concurrent_requests: Factor<number>;
  content_bytes: Factor<number>;
  destination: Factor<"session" | "publish">;
  target_mode: Factor<"independent" | "same_target">;
}

export interface FileEditFactors {
  concurrent_requests: Factor<number>;
  file_bytes: Factor<number>;
  replacement_count: Factor<number>;
  match_density: Factor<number>;
  destination: Factor<"session" | "publish">;
  target_mode: Factor<"independent" | "same_target">;
}

export interface FileBlameFactors {
  concurrent_requests: Factor<number>;
  line_count: Factor<number>;
  ownership_segments: Factor<number>;
  auditability_event_count: Factor<number>;
}

export interface CreateWorkspaceFactors {
  workspace_count: Factor<number>;
  workspace_profile: Factor<WorkspaceProfileId>;
  network_profile: Factor<AllowedNetworkProfile>;
}

export interface SquashLayerstackFactors {
  live_sessions: Factor<number>;
  requested_migration_ratio: Factor<number>;
  remount_parallelism: Factor<number>;
  squashable_blocks: Factor<number>;
  layers_per_block: Factor<number>;
  payload_bytes: Factor<number>;
  session_activity: Factor<"idle" | "active">;
}

interface OperationConfiguration<TFactors> {
  enabled: boolean;
  factors: TFactors;
}

export type OperationPlan =
  | { operation: "exec_command"; configuration: OperationConfiguration<ExecCommandFactors> }
  | { operation: "file_read"; configuration: OperationConfiguration<FileReadFactors> }
  | { operation: "file_write"; configuration: OperationConfiguration<FileWriteFactors> }
  | { operation: "file_edit"; configuration: OperationConfiguration<FileEditFactors> }
  | { operation: "file_blame"; configuration: OperationConfiguration<FileBlameFactors> }
  | {
      operation: "create_workspace";
      configuration: OperationConfiguration<CreateWorkspaceFactors>;
    }
  | {
      operation: "squash_layerstack";
      configuration: OperationConfiguration<SquashLayerstackFactors>;
    };

export type TreatmentField =
  | "source_commit"
  | "source_diff_hash"
  | "daemon_binary_hash"
  | "gateway_binary_hash";

export interface ExperimentPlan {
  schema_version: number;
  name: string;
  configuration_base: {
    id: string;
    version: number;
    scope: ConfigurationScope;
  };
  seed: number;
  environment: {
    image: string;
    client_cohort: ClientCohort;
  };
  protocol: {
    order: "randomized_blocks";
    resource_interval_ms: number;
    trial_defaults: {
      fast: { warmups: number; measured_trials: number };
      destructive: { warmups: number; measured_trials: number };
    };
    timeout_ms: {
      default: number;
      squash_layerstack: number;
    };
  };
  operations: OperationPlan[];
  comparison: {
    protocol_id: string;
    protocol_version: number;
    treatment_fields: TreatmentField[];
  } | null;
}

export type FactorId =
  | "concurrent_requests"
  | "workspace_profile"
  | "session_mode"
  | "command_case"
  | "returned_bytes"
  | "read_source"
  | "target_mode"
  | "content_bytes"
  | "mutation_destination"
  | "file_bytes"
  | "replacement_count"
  | "match_density"
  | "line_count"
  | "ownership_segments"
  | "auditability_event_count"
  | "workspace_count"
  | "network_profile"
  | "live_sessions"
  | "requested_migration_ratio"
  | "remount_parallelism"
  | "squashable_blocks"
  | "layers_per_block"
  | "payload_bytes"
  | "session_activity";

export type CheckId =
  | "command_exit_status"
  | "command_output"
  | "command_lifecycle"
  | "file_read_window"
  | "file_content_hash"
  | "mutation_attribution"
  | "file_edit_replacement_count"
  | "blame_range_coverage"
  | "blame_ownership"
  | "workspace_ready"
  | "workspace_network_profile"
  | "workspace_registry_baseline"
  | "layerstack_content_equivalence"
  | "layerstack_manifest_reduction"
  | "layerstack_disposition_accounting"
  | "layerstack_session_usability"
  | "layerstack_residue";

export type PhaseId =
  | "layerstack_squash"
  | "layerstack_storage_plan"
  | "layerstack_flatten"
  | "layerstack_commit"
  | "layerstack_remount_sweep"
  | "workspace_session_remount";

export type PhaseUnit = "nanoseconds";
export type PhaseSource = "product_trace";
export type PhaseCorrelationRule = "exact_request_trace_span";

export type CountSemantics =
  | { kind: "concurrent_requests"; factor: FactorId }
  | { kind: "concurrent_workspace_creates"; factor: FactorId }
  | { kind: "single_request_with_prepared_load"; load_factor: FactorId };

export type FactorConstraint =
  | { kind: "positive" }
  | { kind: "non_negative" }
  | { kind: "unit_interval" }
  | { kind: "choices"; values: string[] }
  | { kind: "profile_catalog"; catalog: "workspace_profiles" };

export interface WorkspaceFixtureSpec {
  file_count: number;
  logical_bytes: number;
  maximum_depth: number;
}

export interface WorkspaceProfileEnvelope {
  schema_version: number;
  id: WorkspaceProfileId;
  version: number;
  label: string;
  help: string;
  generator_version: number;
  standard: boolean;
  fixture: WorkspaceFixtureSpec;
}

export interface WorkspaceProfileCatalog {
  schema_version: number;
  profiles: WorkspaceProfileEnvelope[];
}

export interface FactorDefinition {
  id: FactorId;
  label: string;
  help: string;
  value_kind: "unsigned_integer" | "unit_ratio" | "choice";
  unit: "count" | "bytes" | "ratio" | null;
  constraint: FactorConstraint;
  comparison: "scientific_invariant" | "non_scientific";
}

export interface CheckReference {
  id: CheckId;
  label: string;
  help: string;
  semantic_revision: number;
  evidence_limit: number;
}

export interface PhaseReference {
  id: PhaseId;
  label: string;
  help: string;
  semantic_revision: number;
  unit: PhaseUnit;
  source: PhaseSource;
  correlation: PhaseCorrelationRule;
  trace_span_name: string;
}

export type ProductAccess =
  | {
      kind: "public_gateway";
      action:
        | "exec_command"
        | "file_read"
        | "file_write"
        | "file_edit"
        | "file_blame"
        | "squash_layerstacks";
    }
  | { kind: "daemon_http"; action: "file_list" }
  | { kind: "internal_workspace"; action: "create_no_op_session" | "destroy_session" };

export interface FamilyDefinition {
  id: FamilyId;
  label: string;
  help: string;
  research_question: string;
  measured_boundary: string;
}

export interface OperationDefinition {
  id: OperationId;
  family: FamilyId;
  label: string;
  help: string;
  measured_boundary: string;
  count_semantics_help: string;
  semantic_revision: number;
  factor_schema_revision: number;
  count_semantics: CountSemantics;
  execution_shape:
    | "barrier_request_batch"
    | "barrier_workspace_creation"
    | "single_request_after_prepared_load";
  isolation:
    | "session_mode_dependent"
    | "reusable_verified_fixture"
    | "mutation_destination_dependent"
    | "prepared_sandbox_per_cell"
    | "fresh_topology_per_trial";
  cleanup: CleanupPolicy;
  product_access: ProductAccess;
  supported_cohorts: ClientCohort[];
  security_class:
    | "bounded_shell"
    | "public_read_only"
    | "public_mutation"
    | "internal_workspace_lifecycle"
    | "destructive_manager_mutation";
  factors: FactorDefinition[];
  checks: CheckReference[];
  phases: PhaseReference[];
  comparison: {
    semantic_revision: number;
    factors: FactorId[];
  };
}

export interface MetricDefinition {
  id: string;
  semantic_revision: number;
  unit: MetricUnit;
  scope: MetricScope;
  kind: MetricKind;
  availability: AvailabilityPolicy;
  aggregation: AggregationRule;
  direction: MetricDirection;
}

export interface DefinitionCatalog {
  schema_version: number;
  families: FamilyDefinition[];
  factor_roles: ["varied", "controlled"];
  workspace_profiles: WorkspaceProfileCatalog;
  operations: OperationDefinition[];
  metrics: MetricDefinition[];
}

export interface PresetRef {
  id: string;
  version: number;
}

export interface PresetFile extends PresetRef {
  schema_version: number;
  plan: ExperimentPlan;
}

export interface DefinitionsResponse {
  schema_version: number;
  catalog: DefinitionCatalog;
  defaults: ExperimentPlan[];
  presets: PresetFile[];
}

export interface ValidationFinding {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  path: string | null;
}

export interface PlanEstimates {
  cell_count: number;
  trial_batch_count: number;
  issued_operation_request_count: number;
  duration_range: {
    minimum_ns: number;
    maximum_ns: number;
  };
  estimated_peak_disk_bytes: number | null;
  required_free_space_bytes: number | null;
  gateway_restart_count: number;
  warnings: string[];
}

export interface ExecCommandCell {
  concurrent_requests: number;
  workspace_profile: WorkspaceProfileId;
  session_mode: "explicit" | "automatic";
  command_case: "noop" | "output64_kib" | "cpu50_ms" | "fixture_read";
  template_revision: number;
  command: string;
  command_sha256: string;
  expected_exit_code: number;
  output_limit_bytes: number;
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface FileReadCell {
  concurrent_requests: number;
  returned_bytes: number;
  source: "snapshot" | "session";
  target_mode: "independent" | "same_target";
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface FileWriteCell {
  concurrent_requests: number;
  content_bytes: number;
  destination: "session" | "publish";
  target_mode: "independent" | "same_target";
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface FileEditCell {
  concurrent_requests: number;
  file_bytes: number;
  replacement_count: number;
  match_density: number;
  destination: "session" | "publish";
  target_mode: "independent" | "same_target";
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface FileBlameCell {
  concurrent_requests: number;
  line_count: number;
  ownership_segments: number;
  auditability_event_count: number;
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface CreateWorkspaceCell {
  workspace_count: number;
  workspace_profile: WorkspaceProfileId;
  network_profile: AllowedNetworkProfile;
  resolved_isolation: ResolvedIsolationPolicy;
}

export interface SquashLayerstackCell {
  live_sessions: number;
  requested_migration_ratio: number;
  remount_parallelism: number;
  squashable_blocks: number;
  layers_per_block: number;
  payload_bytes: number;
  session_activity: "idle" | "active";
  resolved_isolation: ResolvedIsolationPolicy;
}

export type ExpandedOperationCell =
  | { operation: "exec_command"; cell: ExecCommandCell }
  | { operation: "file_read"; cell: FileReadCell }
  | { operation: "file_write"; cell: FileWriteCell }
  | { operation: "file_edit"; cell: FileEditCell }
  | { operation: "file_blame"; cell: FileBlameCell }
  | { operation: "create_workspace"; cell: CreateWorkspaceCell }
  | { operation: "squash_layerstack"; cell: SquashLayerstackCell };

export interface ExpandedCell {
  cell_id: string;
  family_id: FamilyId;
  operation_id: OperationId;
  operation_semantic_revision: number;
  factor_schema_revision: number;
  protocol: {
    destructive: boolean;
    warmups: number;
    measured_trials: number;
    timeout_ms: number;
    cleanup: CleanupPolicy;
  };
  operation: ExpandedOperationCell;
}

export interface ExecutionBlock {
  block_id: string;
  family_id: FamilyId;
  cell_ids: string[];
  restart_reason: string | null;
}

export interface PlanValidationResponse {
  schema_version: number;
  runnable: boolean;
  is_customized: boolean;
  plan_hash: string;
  canonical_plan: ExperimentPlan;
  effective_environment: {
    test_workspace_root: string;
    workspace_root_identity: string;
    client_cohort: ClientCohort;
    image_digest: string | null;
    filesystem: string | null;
    free_space_bytes: number | null;
    gateway_mode: "isolated";
  };
  fixed_lifecycle_policy: {
    lifecycle_revision: number;
    failure_revision: number;
    stabilization_revision: number;
    automatic_retries: number;
    one_active_campaign: boolean;
    sequential_families: boolean;
  };
  selected_workspace_profiles: WorkspaceProfileEnvelope[];
  cells: ExpandedCell[];
  execution_blocks: ExecutionBlock[];
  estimates: PlanEstimates;
  validation: ValidationFinding[];
}

export interface PlanValidationRequest {
  plan: ExperimentPlan;
  starting_preset: PresetRef | null;
}

export type RunState =
  | "queued"
  | "planned"
  | "preparing"
  | "running"
  | "verifying"
  | "tearing_down"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled";

export interface HealthCheck {
  id: string;
  status: "pass" | "warning" | "fail";
  message: string;
}

export interface HealthResponse {
  schema_version: number;
  status: "ready" | "degraded" | "unready";
  execution_ready: boolean;
  version: string;
  runner_instance_id: string;
  active_run: { run_id: string; state: RunState } | null;
  checks: HealthCheck[];
}

export interface SettingsResponse {
  schema_version: number;
  test_workspace_root: string;
  source: "command_line" | "environment" | "persisted" | "sibling_default" | "api_update";
  writable: boolean;
  path_health: {
    canonical: boolean;
    root_marker: boolean;
    outside_repository: boolean;
  };
}

export interface SettingsUpdateRequest {
  test_workspace_root: string;
}

export interface RunSummary {
  run_id: string;
  name: string;
  state: RunState;
  plan_hash: string;
  configuration_scope: ConfigurationScope;
  source_commit: string;
  source_dirty: boolean;
  started_at: string;
  ended_at: string | null;
  correctness: "pass" | "fail" | "pending";
}

export interface RunListResponse {
  schema_version: number;
  runs: RunSummary[];
  next_cursor: string | null;
}

export interface RunManifestSummary extends RunSummary {
  definition_snapshot_version: number;
  environment_fingerprint: string;
}

export interface RunProgress {
  current_family: FamilyId | null;
  current_operation: OperationId | null;
  current_cell_id: string | null;
  current_trial_id: string | null;
  trial_kind: "warmup" | "measured" | null;
  phase: "setup" | "operation" | "verify" | "teardown" | null;
  completed_trial_batches: number;
  total_trial_batches: number;
  issued_operation_requests: number;
  warning_count: number;
  failure_count: number;
}

export interface RunResponse {
  schema_version: number;
  manifest: RunManifestSummary;
  progress: RunProgress;
  latest_sequence: number;
  report_ready: boolean;
}

export interface RunCreateRequest {
  plan: ExperimentPlan;
  plan_hash: string;
  client_request_id: string;
  starting_preset: PresetRef | null;
}

export interface RunCreateResponse {
  schema_version: number;
  run_id: string;
  state: RunState;
}

export interface RunCancelResponse {
  schema_version: number;
  run_id: string;
  state: RunState;
  cancellation_requested: boolean;
}

export type WorkState =
  | "pending"
  | "preparing"
  | "running"
  | "verifying"
  | "tearing_down"
  | "completed"
  | "failed"
  | "cancelled"
  | "skipped";

export type EventData =
  | { kind: "run_state"; state: RunState }
  | { kind: "family_state"; family: FamilyEventName; state: WorkState }
  | { kind: "cell_state"; cell_id: string; state: WorkState }
  | { kind: "trial_state"; cell_id: string; trial_id: string; warmup: boolean; state: WorkState }
  | {
      kind: "trial_phase";
      cell_id: string;
      trial_id: string;
      warmup: boolean;
      phase: "setup" | "operation" | "verify" | "teardown";
      state: WorkState;
    }
  | {
      kind: "request_state";
      cell_id: string;
      trial_id: string;
      request_id: string;
      state: "waiting_at_barrier" | "in_flight" | "succeeded" | "failed" | "cancelled";
    }
  | {
      kind: "resource_window";
      cell_id: string;
      trial_id: string;
      metric_id: string;
      value: number | null;
      unavailable_reason: string | null;
    }
  | {
      kind: "correctness";
      cell_id: string;
      trial_id: string;
      check_id: string;
      passed: boolean;
      expected: string;
      actual: string;
      artifact_id: string | null;
    }
  | { kind: "warning"; code: string; message: string }
  | { kind: "log"; level: "debug" | "info" | "warn" | "error"; message: string }
  | { kind: "report_ready"; provisional: boolean };

export interface EventRecord {
  sequence: number;
  run_id: string;
  monotonic_offset_ns: number;
  data: EventData;
}

export interface ReportResultRow {
  row_id: string;
  operation_id: OperationId;
  cell_id: string;
  metric_id: string;
  unit: MetricUnit;
  successful_n: number;
  failed_n: number;
  unavailable_n: number;
  median: number | null;
  confidence_interval: ReportConfidenceInterval | null;
  interval_omission_reason: string | null;
  direction: MetricDirection;
}

export type MetricUnit =
  | "bytes"
  | "bytes_per_second"
  | "nanoseconds"
  | "operations_per_second"
  | "count"
  | "ratio";
export type MetricScope =
  | "operation"
  | "host_volume"
  | "runner"
  | "daemon"
  | "sandbox"
  | "workspace"
  | "layerstack";
export type MetricKind = "gauge" | "monotonic_counter";
export type AvailabilityPolicy = "explicit_unavailable";
export type AggregationRule = "maximum" | "minimum" | "mean" | "delta" | "integral";
export type MetricDirection =
  | "lower_is_preferred"
  | "higher_is_preferred"
  | "descriptive_only";

export interface ConfidenceInterval {
  level: number;
  lower: number;
  upper: number;
  method: "percentile_bootstrap_median";
  resamples: number;
}

export type DistributionProjection =
  | { kind: "empty" }
  | { kind: "raw_points"; values: number[] }
  | {
      kind: "histogram_ecdf";
      histogram: {
        method: "freedman_diaconis" | "sturges" | "single_value";
        edges: number[];
        counts: number[];
      };
      ecdf: { value: number; cumulative_probability: number }[];
    };

export interface SampleStatistics {
  schema_version: number;
  count: number;
  minimum: number | null;
  maximum: number | null;
  mean: number | null;
  sample_standard_deviation: number | null;
  median: number | null;
  median_absolute_deviation: number | null;
  p25: number | null;
  p75: number | null;
  p95: number | null;
  coefficient_of_variation: number | null;
  median_confidence_interval: ConfidenceInterval | null;
  confidence_interval_omission: "insufficient_n" | null;
  p95_exploratory: boolean;
  outlier_indices: number[];
  distribution: DistributionProjection;
}

export interface FailureCounts {
  total_attempted: number;
  warmup: number;
  measured_attempted: number;
  successful: number;
  product_failed: number;
  correctness_failed: number;
  infrastructure_failed: number;
  cleanup_invalid: number;
  missing_primary_latency: number;
}

export interface MetricIdentity {
  id: string;
  label: string;
  help: string;
  semantic_revision: number;
  unit: MetricUnit;
  scope: MetricScope;
  kind: MetricKind;
  availability: AvailabilityPolicy;
  aggregation: AggregationRule;
  direction: MetricDirection;
  source: string;
  ratio_scale: boolean;
  report_derivation_revision: number;
}

export interface MetricSummary {
  identity: MetricIdentity;
  attempted_n: number;
  failed_n: number;
  available_n: number;
  unavailable: { count: number; reasons: Record<string, number> };
  statistics: SampleStatistics;
  raw_points: MetricRawPoint[];
}

export interface MetricRawPoint {
  trial_id: string;
  request_id: string | null;
  value: number;
  raw_integer_value: number | null;
  outlier: boolean;
}

export interface CheckSummary {
  id: CheckId;
  label: string;
  help: string;
  semantic_revision: number;
  attempted: number;
  passed: number;
  failed: number;
}

export interface PhaseSummary {
  id: PhaseId;
  label: string;
  help: string;
  semantic_revision: number;
  unit: PhaseUnit;
  source: PhaseSource;
  correlation: PhaseCorrelationRule;
  trace_span_name: string;
  attempted: number;
  failed: number;
  duration: SampleStatistics;
}

export type FactorDisplayUnit = "count" | "bytes" | "ratio";

export type ReportFactorValue =
  | { kind: "unsigned_integer"; value: number }
  | { kind: "ratio"; value: number }
  | { kind: "choice"; value: string };

export interface ReportFactor {
  id: FactorId;
  label: string;
  help: string;
  role: FactorRole;
  unit: FactorDisplayUnit | null;
  value: ReportFactorValue;
  control: ReportFactorValue | null;
}

export interface FactorStudyCell {
  cell_id: string;
  factors: ReportFactor[];
  successful_n: number;
  failed_n: number;
  median: number | null;
  confidence_interval: ReportConfidenceInterval | null;
  interval_omission_reason: string | null;
  raw_points: MetricRawPoint[];
}

export type FactorStudyLayout =
  | { kind: "single_cell" }
  | { kind: "trend"; factor_id: FactorId }
  | { kind: "matrix"; row_factor_id: FactorId; column_factor_id: FactorId }
  | { kind: "small_multiples"; factor_ids: FactorId[] };

export interface FactorStudyProjection {
  operation_id: OperationId;
  operation_label: string;
  metric: MetricIdentity;
  layout: FactorStudyLayout;
  varied_factor_ids: FactorId[];
  controlled_factor_ids: FactorId[];
  cells: FactorStudyCell[];
  control_comparisons: ControlComparisonProjection[];
}

export interface ControlComparisonProjection {
  comparison_id: string;
  control_cell_id: string;
  candidate_cell_id: string;
  changed_factor_ids: FactorId[];
  control_median: number | null;
  candidate_median: number | null;
  absolute_difference: number | null;
  percentage_difference: number | null;
  median_difference_confidence_interval: ReportConfidenceInterval | null;
  interval_omission_reason: string | null;
}

export interface RequestSpanProjection {
  request_id: string;
  start_offset_ns: number;
  duration_ns: number;
  succeeded: boolean;
  status: string;
}

export type PhaseStatus = "succeeded" | "failed" | "cancelled" | "timed_out";

export interface PhaseSpanProjection {
  id: PhaseId;
  label: string;
  help: string;
  semantic_revision: number;
  request_id: string | null;
  start_offset_ns: number;
  duration_ns: number;
  status: PhaseStatus;
}

export interface ResourceTimelinePoint {
  monotonic_offset_ns: number;
  sampled: boolean;
  value: Availability<number>;
}

export interface ResourceSeriesProjection {
  identity: MetricIdentity;
  request_id: string | null;
  points: ResourceTimelinePoint[];
}

export interface ResourceTimelineProjection {
  trial_id: string;
  domain_start_ns: number;
  domain_end_ns: number;
  operation_window: OperationWindowProjection | null;
  request_spans: RequestSpanProjection[];
  phase_spans: PhaseSpanProjection[];
  series: ResourceSeriesProjection[];
}

export interface OperationWindowProjection {
  start_offset_ns: number;
  duration_ns: number;
}

export interface CheckEvidenceItem {
  expected: string;
  actual: string;
  artifact_id: string | null;
}

export interface BoundedCheckEvidence {
  items: CheckEvidenceItem[];
  truncated_count: number;
  truncated_sha256: string | null;
}

export interface CheckEvidenceReport {
  id: CheckId;
  label: string;
  help: string;
  semantic_revision: number;
  trial_id: string;
  request_id: string | null;
  verdict: "pass" | "fail";
  duration_ns: number;
  evidence: BoundedCheckEvidence;
}

export type Availability<T> =
  | { availability: "available"; value: T }
  | { availability: "unavailable"; source: string; reason: string };

export interface BoundedOutputEvidence {
  byte_count: number;
  truncated: boolean;
  sha256: string;
}

export interface ExecCommandEvidence {
  command_case: "noop" | "output64_kib" | "cpu50_ms" | "fixture_read";
  template_revision: number;
  command_sha256: string;
  exit_code: number | null;
  stdout: BoundedOutputEvidence;
  stderr: BoundedOutputEvidence;
}

export interface FileReadEvidence {
  requested_bytes: number;
  returned_bytes: number;
  returned_lines: number;
  content_sha256: string;
}

export type MutationAttribution = "workspace_session" | "published_operation_layer";

export interface FileWriteEvidence {
  requested_bytes: number;
  observed_bytes: number;
  expected_sha256: string;
  observed_sha256: string;
  attribution: MutationAttribution;
  attributed_layer_count: number;
}

export interface FileEditEvidence {
  requested_replacements: number;
  applied_replacements: number;
  before_sha256: string;
  expected_sha256: string;
  observed_sha256: string;
  attribution: MutationAttribution;
  attributed_layer_count: number;
}

export interface FileBlameEvidence {
  requested_lines: number;
  returned_ranges: number;
  covered_lines: number;
  expected_ownership_segments: number;
  matched_ownership_segments: number;
  observed_auditability_events: number;
}

export interface CreateWorkspaceEvidence {
  requested_count: number;
  created_count: number;
  ready_count: number;
  destroyed_count: number;
  network_profile_matches: number;
  registry_baseline_restored: boolean;
}

export interface SessionDispositionCounts {
  migrated: number;
  identity: number;
  leased: number;
  faulty: number;
  session_gone: number;
}

export interface StorageSnapshot {
  monotonic_offset_ns: Availability<number>;
  sampled: boolean;
  manifest_version: Availability<number>;
  root_hash: Availability<string>;
  active_layer_count: Availability<number>;
  active_lease_count: Availability<number>;
  active_logical_bytes: Availability<number>;
  active_allocated_bytes: Availability<number>;
  storage_logical_bytes: Availability<number>;
  storage_allocated_bytes: Availability<number>;
  staging_entry_count: Availability<number>;
}

export interface SourceLayerAllocation {
  layer_id: string;
  logical_bytes: Availability<number>;
  allocated_bytes: Availability<number>;
}

export interface SquashLayerstackEvidence {
  requested_live_sessions: number;
  observed_migrated_sessions: number;
  observed_non_migrated_sessions: number;
  dispositions: SessionDispositionCounts;
  effective_remount_parallelism: number;
  observed_squashed_block_count: number;
  observed_replaced_layer_count: number;
  source_layer_ids: string[];
  retained_source_layer_ids: string[];
  source_layer_allocations: SourceLayerAllocation[];
  reclaimed_bytes: Availability<number>;
  s0_baseline: StorageSnapshot;
  s1_sampled_peak: StorageSnapshot;
  s2_post_commit: StorageSnapshot;
  s3_settled: StorageSnapshot;
  manifest_reduced: boolean;
  content_equivalent: boolean;
  usable_session_count: number;
}

export type OperationEvidence =
  | { operation: "exec_command"; evidence: ExecCommandEvidence }
  | { operation: "file_read"; evidence: FileReadEvidence }
  | { operation: "file_write"; evidence: FileWriteEvidence }
  | { operation: "file_edit"; evidence: FileEditEvidence }
  | { operation: "file_blame"; evidence: FileBlameEvidence }
  | { operation: "create_workspace"; evidence: CreateWorkspaceEvidence }
  | { operation: "squash_layerstack"; evidence: SquashLayerstackEvidence };

type OperationObservationFor<TEvidence extends OperationEvidence> =
  TEvidence extends OperationEvidence
    ? {
        operation_id: TEvidence["operation"];
        cell_id: string;
        trial_id: string;
        request_id: string | null;
        evidence: TEvidence;
      }
    : never;

export type OperationObservation = OperationObservationFor<OperationEvidence>;

export interface OperationObservationRecord {
  record: "operation";
  data: OperationObservation;
}

export interface OperationEvidenceReport {
  trial_id: string;
  request_id: string | null;
  evidence: OperationEvidence;
}

export interface CpuLatencyCorrelation {
  semantic_revision: number;
  method: "pearson";
  alignment: "eligible_trial_aggregate_by_trial_id";
  eligibility: "measured_product_success_checks_pass_cleanup_restored";
  latency_metric_id: string;
  cpu_metric_id: string;
  support_count: number;
  coefficient: number | null;
  confidence_interval: PearsonConfidenceInterval | null;
  interval_omission:
    | "insufficient_n"
    | "zero_variance"
    | "insufficient_valid_resamples"
    | null;
  points: {
    trial_id: string;
    operation_latency_ns: number;
    sandbox_cpu_time_ns: number;
  }[];
  exclusions: {
    ineligible_trial: number;
    missing_latency: number;
    missing_cpu: number;
    unavailable_cpu: number;
  };
}

export interface PearsonConfidenceInterval {
  level: number;
  lower: number;
  upper: number;
  method: "percentile_bootstrap_pearson";
  resamples: number;
  valid_resamples: number;
}

export interface CellSummary {
  cell_id: string;
  family_id: FamilyId;
  family_label: string;
  operation_id: OperationId;
  operation_label: string;
  comparison_key: Record<string, unknown>;
  design_counts: ReportDesignCounts;
  factors: ReportFactor[];
  counts: FailureCounts;
  metrics: MetricSummary[];
  checks: CheckSummary[];
  phases: PhaseSummary[];
  timelines: ResourceTimelineProjection[];
  check_evidence: CheckEvidenceReport[];
  operation_evidence: OperationEvidenceReport[];
  cpu_latency_correlation: CpuLatencyCorrelation;
}

export type ReportConfidenceMethod =
  | "percentile_bootstrap_median"
  | "percentile_bootstrap_median_difference";

export interface ReportConfidenceInterval {
  level: number;
  lower: number;
  upper: number;
  method: ReportConfidenceMethod;
  resamples: number;
}

export interface ReportDesignCounts {
  test_combinations: number;
  trial_batches: number;
  issued_product_requests: number;
}

export type CellOrder = "randomized_blocks";

export interface TreatmentIdentity {
  source_commit: string;
  source_dirty: boolean;
  source_diff_hash: string | null;
  daemon_binary_hash: string | null;
  gateway_binary_hash: string | null;
}

export interface HostEnvironment {
  operating_system: string;
  architecture: string;
  kernel_release: string | null;
  docker_engine_version: string | null;
  filesystem: string | null;
  monotonic_clock: string;
}

export interface EnvironmentMetadata {
  schema_version: number;
  treatment: TreatmentIdentity;
  host: HostEnvironment;
  image_reference: string;
  image_digest: string | null;
  workspace_root_identity: string;
  client_cohort: ClientCohort;
  gateway_endpoint_identity: string;
}

export interface ProducerIdentity {
  package: string;
  version: string;
}

export interface ArtifactSchemaIdentity {
  schema_name: string;
  write_version: number;
  read_versions: number[];
}

export interface ArtifactSchemaSet {
  run_manifest: ArtifactSchemaIdentity;
  intent_plan: ArtifactSchemaIdentity;
  expanded_plan: ArtifactSchemaIdentity;
  definition_snapshot: ArtifactSchemaIdentity;
  environment_metadata: ArtifactSchemaIdentity;
  events: ArtifactSchemaIdentity;
  observations: ArtifactSchemaIdentity;
  bounded_evidence: ArtifactSchemaIdentity;
}

export interface MetricRevisionIdentity {
  metric_id: string;
  semantic_revision: number;
}

export interface CheckRevisionIdentity {
  check_id: CheckId;
  semantic_revision: number;
}

export interface PhaseRevisionIdentity {
  phase_id: PhaseId;
  semantic_revision: number;
}

export type StabilizationPolicy =
  | { kind: "not_required"; semantic_revision: number }
  | {
      kind: "exact_snapshot_quiet_window";
      semantic_revision: number;
      quiet_window_matches: number;
      poll_interval_ms: number;
      timeout_ms: number;
    };

export interface OperationAuthority {
  operation_id: OperationId;
  family_id: FamilyId;
  semantic_revision: number;
  factor_schema_revision: number;
  comparison_projection_revision: number;
  client_cohort: ClientCohort;
  product_access: ProductAccess;
  count_semantics: CountSemantics;
  cleanup_policy: CleanupPolicy;
  resolved_isolation_policies: ResolvedIsolationPolicy[];
  request_timeout_ms: number[];
  stabilization_policy: StabilizationPolicy;
}

export interface MethodsReport {
  schema_version: number;
  report_derivation_revision: number;
  artifact_reader_revision: number;
  plan_schema_version: number;
  plan_seed: number;
  cell_order: CellOrder;
  resource_sample_interval_ms: number;
  design_counts: ReportDesignCounts;
  fixture_generator_revision: number;
  fixture_hashes: Record<string, string>;
  producer: ProducerIdentity;
  artifact_schemas: ArtifactSchemaSet;
  operation_authorities: OperationAuthority[];
  metric_revisions: MetricRevisionIdentity[];
  derived_metric_revisions: MetricRevisionIdentity[];
  check_revisions: CheckRevisionIdentity[];
  phase_revisions: PhaseRevisionIdentity[];
  environment: EnvironmentMetadata;
  raw_time_unit: string;
  monotonic_clock: string;
  quantile_interpolation: string;
  confidence_interval: string;
  bootstrap_resamples: number;
  outlier_policy: string;
  warmup_policy: string;
  failure_policy: string;
  resource_policy: string;
  comparison_policy: string;
}

export interface ReportWarning {
  code: string;
  message: string;
}

export interface RunDerivedSummary {
  schema_version: 4;
  report_derivation_revision: number;
  run_id: string;
  plan_hash: string;
  state: RunState;
  provisional: boolean;
  correctness_verdict: "pass" | "fail" | "pending";
  design_counts: ReportDesignCounts;
  cells: CellSummary[];
  warnings: ReportWarning[];
}

export interface ReportResponse {
  schema_version: 4;
  report_derivation_revision: number;
  run_id: string;
  state: RunState;
  provisional: boolean;
  correctness_verdict: "pass" | "fail" | "pending";
  design_counts: ReportDesignCounts;
  research_question: string;
  plan_hash: string;
  source_commit: string;
  source_dirty: boolean;
  environment_fingerprint: string;
  definition_snapshot_version: number;
  definition_snapshot_sha256: string;
  started_at: string | null;
  ended_at: string | null;
  summary: ReportResultRow[];
  factor_studies: FactorStudyProjection[];
  cells: CellSummary[];
  methods: MethodsReport;
  limitations: string[];
  warnings: ReportWarning[];
}

export interface ArtifactIndexEntry {
  artifact_id: string;
  label: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
}

export interface ArtifactIndexResponse {
  schema_version: number;
  run_id: string;
  artifacts: ArtifactIndexEntry[];
}

export interface ArtifactContentResponse extends ArtifactIndexEntry {
  schema_version: number;
  encoding: "utf-8" | "base64";
  content: string;
}

export interface ComparisonRequest {
  reference_run_id: string;
  candidate_run_id: string;
  descriptive_override: boolean;
}

export interface CompatibilityCheck {
  check_id: string;
  label: string;
  compatible: boolean;
  consequence: string;
  scope: "core_invariant" | "treatment" | "metric" | "correctness" | "phase";
  blocks_aggregate: boolean;
}

export interface NormalizedComparisonPlan {
  protocol_id: string;
  protocol_version: number;
  treatment_fields: TreatmentField[];
  source: "defaulted" | "explicit";
}

export interface TreatmentDifference {
  field: TreatmentField;
  identity_component: string;
  reference: string | null;
  candidate: string | null;
  declared: boolean;
}

export interface MatchedCell {
  match_id: string;
  comparison_key_sha256: string;
  operation_id: OperationId;
  reference_cell_id: string;
  candidate_cell_id: string;
  effective_protocol_compatible: boolean;
}

export interface ComparisonDelta {
  comparison_id: string;
  match_id: string;
  reference_cell_id: string;
  candidate_cell_id: string;
  metric_id: string;
  unit: MetricUnit;
  reference_unit: MetricUnit | null;
  candidate_unit: MetricUnit | null;
  reference_value: number | null;
  candidate_value: number | null;
  reference_n: number;
  candidate_n: number;
  reference_unavailable_n: number;
  candidate_unavailable_n: number;
  absolute_change: number | null;
  percent_change: number | null;
  median_difference_confidence_interval: ConfidenceInterval | null;
  confidence_interval_omission_reason: string | null;
  unavailable_reason: string | null;
  direction: MetricDirection;
  descriptive_only: boolean;
  correctness: {
    reference_correctness_failed: number;
    candidate_correctness_failed: number;
    reference_cleanup_invalid: number;
    candidate_cleanup_invalid: number;
  };
}

export interface ComparisonResponse {
  schema_version: number;
  comparison_derivation_revision: number;
  reference_run_id: string;
  candidate_run_id: string;
  protocol: {
    reference: NormalizedComparisonPlan;
    candidate: NormalizedComparisonPlan;
    declarations_compatible: boolean;
  };
  compatible: boolean;
  descriptive_only: boolean;
  treatment_differences: string[];
  typed_treatment_differences: TreatmentDifference[];
  checks: CompatibilityCheck[];
  matched_cell_ids: string[];
  matched_cells: MatchedCell[];
  deltas: ComparisonDelta[];
  performance_verdict: string | null;
}

export interface ApiErrorBody {
  error: {
    code: string;
    message: string;
    details: unknown;
    request_id: string;
  };
}
