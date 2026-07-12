import { describe, expect, it } from "vitest";
import {
  DEFINITIONS_FIXTURE,
  FIXTURE_ROUTE,
  LAYERSTACK_OPERATION_EVIDENCE_FIXTURE,
  LAYERSTACK_OPERATION_OBSERVATION_FIXTURE,
  LAYERSTACK_PHASES,
  UI_FIXTURE_NAMES,
  comparisonFixture,
  reportFixture,
  statisticsFixture,
} from "../fixtures/laboratory";

const documentedFixtures = [
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

describe("documented UI fixture contract", () => {
  it("registers every minimum named fixture exactly once on a documented route", () => {
    expect(UI_FIXTURE_NAMES).toEqual(documentedFixtures);
    expect(new Set(UI_FIXTURE_NAMES).size).toBe(UI_FIXTURE_NAMES.length);
    expect(Object.keys(FIXTURE_ROUTE)).toEqual([...documentedFixtures]);
    expect(new Set(Object.values(FIXTURE_ROUTE).map((route) => route.split("?")[0]))).toEqual(new Set([
      "/benchmark",
      "/benchmark/command",
      "/benchmark/files",
      "/benchmark/workspace",
      "/benchmark/layerstack",
      "/benchmark/runs/run-fixture",
      "/benchmark/reports/run-reference",
      "/benchmark/compare",
    ]));
  });

  it("advertises only cohorts supported by every current backend operation", () => {
    expect(DEFINITIONS_FIXTURE.catalog.operations).toHaveLength(7);
    for (const operation of DEFINITIONS_FIXTURE.catalog.operations) {
      expect(operation.supported_cohorts).toEqual(["direct_client"]);
    }
  });

  it("preserves the complete server-authored phase identity", () => {
    const operation = DEFINITIONS_FIXTURE.catalog.operations.find(({ id }) => id === "squash_layerstack");
    expect(operation?.phases).toEqual(LAYERSTACK_PHASES);
    expect(operation?.phases).toHaveLength(6);
    for (const phase of operation?.phases ?? []) {
      expect(phase).toMatchObject({
        semantic_revision: 1,
        unit: "nanoseconds",
        source: "product_trace",
        correlation: "exact_request_trace_span",
      });
      expect(phase.trace_span_name).not.toBe("");
    }
  });

  it("keeps operation observations correlated to their cell, trial, and product request", () => {
    expect(LAYERSTACK_OPERATION_OBSERVATION_FIXTURE).toMatchObject({
      record: "operation",
      data: {
        operation_id: "squash_layerstack",
        cell_id: "cell-layerstack",
        trial_id: "trial-layerstack-measured-0001",
        request_id: "request-layerstack-0001",
        evidence: { operation: "squash_layerstack" },
      },
    });
  });

  it("keeps unavailable LayerStack allocation counters distinct from zero", () => {
    const evidence = LAYERSTACK_OPERATION_EVIDENCE_FIXTURE.evidence;
    expect(evidence.operation).toBe("squash_layerstack");
    if (evidence.operation !== "squash_layerstack") throw new Error("fixture operation changed");
    const allocation = evidence.evidence.source_layer_allocations[1]?.allocated_bytes;
    expect(allocation).toEqual({
      availability: "unavailable",
      source: "filesystem_allocation_probe",
      reason: "allocated-byte counter unavailable for this snapshot",
    });
    expect(allocation && "value" in allocation).toBe(false);
  });

  it.each([
    [0, "empty", false, true],
    [4, "raw_points", false, true],
    [5, "raw_points", true, true],
    [19, "raw_points", true, true],
    [20, "raw_points", true, false],
    [29, "raw_points", true, false],
    [30, "histogram_ecdf", true, false],
  ] as const)(
    "uses backend projections at n=%i",
    (count, projection, hasInterval, p95Exploratory) => {
      const statistics = statisticsFixture(count);
      expect(statistics.distribution.kind).toBe(projection);
      expect(statistics.median_confidence_interval !== null).toBe(hasInterval);
      expect(statistics.p95_exploratory).toBe(p95Exploratory);
    },
  );

  it("keeps unavailable counters explicit instead of presenting them as zero", () => {
    const report = reportFixture("report-partial-unavailable-resource");
    const cpu = report.cells[0]?.metrics.find(({ identity }) => identity.id === "sandbox_cpu_time_ns");
    expect(cpu?.available_n).toBe(0);
    expect(cpu?.unavailable.count).toBe(29);
    expect(cpu?.statistics.distribution.kind).toBe("empty");
  });

  it("projects the complete schema-v4 report identity without browser derivation", () => {
    const report = reportFixture("report-complete-n30");
    expect(report.schema_version).toBe(4);
    expect(report.report_derivation_revision).toBe(3);
    expect(report.design_counts).toEqual({
      test_combinations: 4,
      trial_batches: 66,
      issued_product_requests: 159,
    });
    expect(report.methods.design_counts).toEqual(report.design_counts);
    expect(report.factor_studies.map(({ layout }) => layout.kind)).toEqual(["trend", "matrix"]);

    const command = report.cells.find(({ cell_id }) => cell_id === "cell-command");
    expect(command?.design_counts).toEqual({
      test_combinations: 1,
      trial_batches: 31,
      issued_product_requests: 124,
    });
    expect(command?.factors.filter(({ role }) => role === "varied").map(({ id }) => id)).toEqual(["concurrent_requests"]);
    expect(command?.factors.find(({ id }) => id === "concurrent_requests")?.control).toEqual({
      kind: "unsigned_integer",
      value: 1,
    });
    expect(command?.factors.filter(({ role }) => role === "controlled").map(({ id }) => id)).toEqual([
      "workspace_profile",
      "session_mode",
      "command_case",
    ]);
    expect(command?.factors.filter(({ role }) => role === "controlled").every(({ control }) => control === null)).toBe(true);
    expect(command?.metrics.slice(0, 6).map(({ identity }) => identity.id)).toEqual([
      "batch_makespan_ns",
      "request_latency_ns",
      "throughput_ops_s",
      "setup_ns",
      "verify_ns",
      "teardown_ns",
    ]);
    for (const metric of command?.metrics ?? []) {
      expect(metric.identity.label).not.toBe("");
      expect(metric.identity.help).not.toBe("");
      expect(metric.identity.report_derivation_revision).toBe(3);
      expect(metric.raw_points).toHaveLength(metric.available_n);
    }
    const batchMakespan = command?.metrics.find(({ identity }) => identity.id === "batch_makespan_ns");
    expect(batchMakespan?.raw_points[0]).toMatchObject({
      trial_id: "trial-command-0001",
      request_id: null,
      raw_integer_value: 963_100,
    });
    const requestLatency = command?.metrics.find(({ identity }) => identity.id === "request_latency_ns");
    expect(requestLatency?.raw_points[0]?.request_id).toBe("request-command-0001-01");
    expect(requestLatency?.raw_points[0]?.raw_integer_value).toBeTypeOf("number");
    const throughput = command?.metrics.find(({ identity }) => identity.id === "throughput_ops_s");
    expect(throughput?.identity.unit).toBe("operations_per_second");
    expect(throughput?.raw_points[0]?.raw_integer_value).toBeNull();
    expect(report.factor_studies[0]?.metric).toEqual(batchMakespan?.identity);
    expect(report.factor_studies[0]?.control_comparisons[0]).toMatchObject({
      control_cell_id: "cell-command-control",
      candidate_cell_id: "cell-command",
      changed_factor_ids: ["concurrent_requests"],
      median_difference_confidence_interval: {
        level: 0.95,
        method: "percentile_bootstrap_median_difference",
        resamples: 10_000,
      },
    });
    expect(command?.timelines[0]?.operation_window).toEqual({
      start_offset_ns: 2_000_000,
      duration_ns: 5_000_000,
    });
    expect(command?.timelines[0]?.request_spans[0]).toMatchObject({
      request_id: "request-command-0001",
      start_offset_ns: 2_000_000,
      duration_ns: 5_000_000,
    });
    const unavailableSample = command?.timelines[0]?.series[0]?.points.find(({ value }) => value.availability === "unavailable");
    expect(unavailableSample?.value).toEqual({
      availability: "unavailable",
      source: "sandbox_resource_counter",
      reason: "sample outside the exact request window",
    });
    expect(unavailableSample?.value && "value" in unavailableSample.value).toBe(false);

    const check = command?.check_evidence[0];
    expect(check?.evidence).toMatchObject({
      truncated_count: 2,
      truncated_sha256: "f7a44f3e6c6ab02a33d887e4746bb00f105e0415ce54a362e46e5a365f0c06fd",
    });
    expect(check?.evidence.items[1]).toMatchObject({ artifact_id: "checks/command-output.json" });

    const layerstack = report.cells.find(({ cell_id }) => cell_id === "cell-layerstack");
    expect(layerstack?.timelines[0]?.phase_spans).toHaveLength(6);
    expect(layerstack?.timelines[0]?.phase_spans[0]).toMatchObject({
      id: "layerstack_squash",
      label: "Total squash",
      start_offset_ns: 1_000_000,
      duration_ns: 9_000_000,
    });
    expect(layerstack?.timelines[0]?.operation_window).toEqual({
      start_offset_ns: 1_000_000,
      duration_ns: 9_000_000,
    });

    expect(report.methods).toMatchObject({
      schema_version: 1,
      report_derivation_revision: 3,
      artifact_reader_revision: 1,
      cell_order: "randomized_blocks",
      fixture_generator_revision: 1,
      producer: { package: "sandbox-benchmark", version: "0.1.0" },
      raw_time_unit: "integer_nanoseconds",
      monotonic_clock: "std::time::Instant",
      quantile_interpolation: "linear_type_7_v1",
      confidence_interval: "deterministic_percentile_bootstrap_median_95_percent",
      bootstrap_resamples: 10_000,
    });
    expect(report.methods.derived_metric_revisions.map(({ metric_id }) => metric_id)).toEqual([
      "batch_makespan_ns",
      "request_latency_ns",
      "throughput_ops_s",
      "setup_ns",
      "verify_ns",
      "teardown_ns",
    ]);
    expect(report.methods.artifact_schemas.observations).toEqual({
      schema_name: "eos_benchmark_observation",
      write_version: 3,
      read_versions: [1, 2, 3],
    });
    expect(report.methods.environment).toMatchObject({
      client_cohort: "direct_client",
      gateway_endpoint_identity: "isolated_loopback_per_execution_block",
      host: { monotonic_clock: "std::time::Instant" },
      treatment: {
        source_commit: "1111111",
        daemon_binary_hash: "daemon-binary-sha256",
        gateway_binary_hash: "gateway-binary-sha256",
      },
    });
  });

  it("publishes the typed Pearson identity and deterministic interval", () => {
    const report = reportFixture("report-cpu-latency-correlation");
    const correlation = report.cells.find(({ cell_id }) => cell_id === "cell-command")?.cpu_latency_correlation;
    expect(correlation).toMatchObject({
      semantic_revision: 1,
      method: "pearson",
      alignment: "eligible_trial_aggregate_by_trial_id",
      eligibility: "measured_product_success_checks_pass_cleanup_restored",
      latency_metric_id: "batch_makespan_ns",
      cpu_metric_id: "sandbox_cpu_time_ns",
      support_count: 5,
      coefficient: 0.75,
      confidence_interval: {
        level: 0.95,
        lower: 0.31,
        upper: 0.92,
        method: "percentile_bootstrap_pearson",
        resamples: 10_000,
        valid_resamples: 9_978,
      },
      interval_omission: null,
    });
    expect(correlation?.points).toHaveLength(5);
  });

  it("keeps mismatch reasons and suppresses verdicts under descriptive override", () => {
    const comparison = comparisonFixture("compare-descriptive-override", true);
    expect(comparison.compatible).toBe(false);
    expect(comparison.descriptive_only).toBe(true);
    expect(comparison.treatment_differences).not.toEqual([]);
    expect(comparison.deltas.every(({ descriptive_only }) => descriptive_only)).toBe(true);
    expect(comparison.performance_verdict).toBeNull();
  });
});
