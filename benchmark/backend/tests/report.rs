mod support;

use std::path::Path;

use sandbox_benchmark::artifacts::{ArtifactError, ArtifactId, ArtifactStore};
use sandbox_benchmark::checks::{
    bounded_evidence, CheckEvidenceItem, CheckResult, CheckVerdict, CorrectnessFold,
};
use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::definitions::{catalog, definition};
use sandbox_benchmark::events::RunState;
use sandbox_benchmark::executors::layerstack::{
    SessionDispositionCounts, SourceLayerAllocation, SquashLayerstackEvidence, StorageSnapshot,
};
use sandbox_benchmark::model::{ConfigurationScope, OperationEvidence, OperationId};
use sandbox_benchmark::plan::{
    load_plan, slice_default, validate_and_expand, ExpandedCell, ExpandedPlan,
};
use sandbox_benchmark::report::{
    regenerate, BenchmarkReport, CorrelationAlignment, CorrelationEligibility, CorrelationMethod,
    FactorStudyLayout, ReportError, ReportFactorValue, CSV_EXPORT_SCHEMA_VERSION,
    JSON_EXPORT_SCHEMA_NAME, JSON_EXPORT_SCHEMA_VERSION, PRIMARY_LATENCY_METRIC_ID,
    REQUEST_LATENCY_METRIC_ID, SETUP_METRIC_ID, TEARDOWN_METRIC_ID, THROUGHPUT_METRIC_ID,
    VERIFY_METRIC_ID,
};
use sandbox_benchmark::resources::{Availability, MetricUnit, ResourceReading, METRICS};
use sandbox_benchmark::scheduler::{
    EnvironmentMetadata, HostEnvironment, LifecycleDurations, ObservationRecord,
    OperationObservation, PhaseObservation, PhaseStatus, RequestObservation, ResourceObservation,
    RunManifest, SequencedObservation, TreatmentIdentity, TrialKind, TrialSample,
    DEFINITION_SNAPSHOT_SCHEMA_NAME, EXPANDED_PLAN_SCHEMA_NAME, OBSERVATION_SCHEMA_NAME,
    OBSERVATION_SCHEMA_VERSION, RUN_MANIFEST_SCHEMA_NAME, RUN_MANIFEST_SCHEMA_VERSION,
};
use sandbox_benchmark::statistics::PearsonConfidenceIntervalOmission;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use support::TestRoot;

struct PreparedRun {
    _root: TestRoot,
    store: ArtifactStore,
    expanded: ExpandedPlan,
}

fn prepare_run(snapshot: Value) -> PreparedRun {
    prepare_run_for_scope(snapshot, ConfigurationScope::Command)
}

fn prepare_run_for_scope(snapshot: Value, scope: ConfigurationScope) -> PreparedRun {
    let root = TestRoot::new("report-regeneration");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical repository");
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repo)
        .expect("initialize benchmark paths");
    let declared =
        load_plan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml"))
            .expect("load standard default");
    let plan = slice_default(&declared, scope);
    let expanded = validate_and_expand(&plan, &paths, Some(&plan)).expect("expand scoped plan");
    assert!(expanded.runnable);
    let definition_snapshot_version = snapshot["schema_version"]
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .expect("definition snapshot schema version");

    let store = ArtifactStore::new(&paths.results).expect("create artifact store");
    store.create_run("run-1").expect("create run");
    store
        .write_immutable(
            "run-1",
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            expanded.schema_version,
            &expanded,
        )
        .expect("write expanded plan");
    store
        .write_immutable(
            "run-1",
            ArtifactId::DefinitionSnapshot,
            DEFINITION_SNAPSHOT_SCHEMA_NAME,
            definition_snapshot_version,
            &snapshot,
        )
        .expect("write definition snapshot");

    let treatment = TreatmentIdentity {
        source_commit: "0123456789abcdef".to_owned(),
        source_dirty: false,
        source_diff_hash: None,
        daemon_binary_hash: Some("sha256:daemon".to_owned()),
        gateway_binary_hash: Some("sha256:gateway".to_owned()),
    };
    let environment = EnvironmentMetadata {
        schema_version: 1,
        treatment: treatment.clone(),
        host: HostEnvironment {
            operating_system: "test".to_owned(),
            architecture: "test".to_owned(),
            kernel_release: Some("test".to_owned()),
            docker_engine_version: Some("test".to_owned()),
            filesystem: expanded.effective_environment.filesystem.clone(),
            free_space_bytes: expanded.effective_environment.free_space_bytes,
            monotonic_clock: "std::time::Instant".to_owned(),
        },
        image_reference: expanded.canonical_plan.environment.image.0.clone(),
        image_digest: expanded.effective_environment.image_digest.clone(),
        workspace_root_identity: expanded
            .effective_environment
            .workspace_root_identity
            .clone(),
        client_cohort: expanded.effective_environment.client_cohort,
        gateway_endpoint_identity: "isolated-test-gateway".to_owned(),
    };
    let definition_snapshot_sha256 = format!(
        "sha256:{:x}",
        Sha256::digest(
            &store
                .content("run-1", ArtifactId::DefinitionSnapshot.as_str())
                .expect("read definition snapshot")
                .bytes
        )
    );
    let mut manifest = RunManifest::planned(
        "run-1",
        &expanded,
        None,
        environment,
        &catalog(),
        definition_snapshot_sha256,
    )
    .expect("build authoritative run manifest");
    manifest.state = RunState::Completed;
    manifest.created_at = "2026-07-12T00:00:00Z".to_owned();
    manifest.started_at = Some("2026-07-12T00:00:01Z".to_owned());
    manifest.ended_at = Some("2026-07-12T00:00:02Z".to_owned());
    store
        .replace_snapshot(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        )
        .expect("write run manifest");

    PreparedRun {
        _root: root,
        store,
        expanded,
    }
}

fn append_observation(prepared: &PreparedRun, sequence: u64, record: ObservationRecord) {
    prepared
        .store
        .append_record(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
            &SequencedObservation { sequence, record },
        )
        .expect("append authoritative observation");
}

fn successful_measured_trial(cell: &ExpandedCell, trial_id: &str) -> TrialSample {
    TrialSample {
        operation_id: cell.operation_id,
        cell_id: cell.cell_id.clone(),
        trial_id: trial_id.to_owned(),
        kind: TrialKind::Measured,
        sequence_in_cell: 1,
        lifecycle: LifecycleDurations {
            setup_ns: 1,
            operation_ns: 10,
            verify_ns: 1,
            teardown_ns: 1,
        },
        product_succeeded: true,
        infrastructure_failed: false,
        cleanup_baseline_restored: true,
        correctness: CorrectnessFold {
            product_succeeded: true,
            required_check_count: 0,
            attempted_check_count: 0,
            passed_check_count: 0,
            failed_check_count: 0,
            missing_checks: Vec::new(),
            unexpected_checks: Vec::new(),
            eligible_for_latency: true,
        },
        primary_operation_latency_ns: Some(10),
        artifacts: Vec::new(),
    }
}

fn request_observation(
    cell: &ExpandedCell,
    trial_id: &str,
    request_id: &str,
) -> RequestObservation {
    RequestObservation {
        operation_id: cell.operation_id,
        cell_id: cell.cell_id.clone(),
        trial_id: trial_id.to_owned(),
        request_id: request_id.to_owned(),
        start_offset_ns: 5,
        latency_ns: 10,
        succeeded: true,
        status: "ok".to_owned(),
        response_bytes: 0,
        bounded_response_sha256: None,
    }
}

fn available<T>(value: T) -> Availability<T> {
    Availability::Available { value }
}

fn storage_snapshot(offset_ns: u64, allocated_bytes: u64) -> StorageSnapshot {
    StorageSnapshot {
        monotonic_offset_ns: available(offset_ns),
        sampled: true,
        manifest_version: available(offset_ns / 10 + 1),
        root_hash: available(format!("sha256:root-{offset_ns}")),
        active_layer_count: available(4),
        active_lease_count: available(1),
        active_logical_bytes: available(8_192),
        active_allocated_bytes: available(allocated_bytes),
        storage_logical_bytes: available(9_216),
        storage_allocated_bytes: available(allocated_bytes + 1_024),
        staging_entry_count: available(0),
    }
}

#[test]
fn manifest_authority_covers_the_closed_catalog_and_regenerates() {
    let prepared = prepare_run_for_scope(
        serde_json::to_value(catalog()).expect("serialize catalog"),
        ConfigurationScope::All,
    );
    let manifest: RunManifest = prepared
        .store
        .read_envelope(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read authoritative manifest");
    assert_eq!(
        manifest
            .operation_authorities
            .iter()
            .map(|authority| authority.operation_id)
            .collect::<Vec<_>>(),
        OperationId::ALL
    );
    for authority in &manifest.operation_authorities {
        let expected = definition(authority.operation_id);
        assert_eq!(authority.family_id, expected.family);
        assert_eq!(authority.semantic_revision, expected.semantic_revision);
        assert_eq!(
            authority.factor_schema_revision,
            expected.factor_schema_revision
        );
        assert_eq!(
            authority.comparison_projection_revision,
            expected.comparison.semantic_revision
        );
        assert_eq!(authority.product_access, expected.product_access);
        assert_eq!(authority.count_semantics, expected.count_semantics);
        assert!(!authority.resolved_isolation_policies.is_empty());
        assert!(!authority.request_timeout_ms.is_empty());
    }
    assert_eq!(manifest.metric_revisions.len(), METRICS.len());
    assert!(!manifest.check_revisions.is_empty());
    assert!(!manifest.phase_revisions.is_empty());
    assert_eq!(
        manifest.artifact_schemas.observations.write_version,
        OBSERVATION_SCHEMA_VERSION
    );
    assert_eq!(
        manifest.artifact_schemas.observations.read_versions,
        vec![1, 2, 3]
    );
    assert_eq!(
        manifest.artifact_schemas.bounded_evidence.schema_name,
        sandbox_benchmark::artifacts::BOUNDED_EVIDENCE_SCHEMA_NAME
    );
    assert!(manifest.gateway_policy.loopback_only);
    assert!(!manifest.gateway_policy.remount_sweep_widths.is_empty());
    assert_eq!(
        manifest
            .safety_policy
            .campaign_caps
            .expanded_test_combinations
            .requested,
        prepared.expanded.estimates.cell_count
    );
    assert_eq!(
        manifest.definition_snapshot.sha256,
        format!(
            "sha256:{:x}",
            Sha256::digest(
                &prepared
                    .store
                    .content("run-1", ArtifactId::DefinitionSnapshot.as_str())
                    .expect("read definition snapshot")
                    .bytes
            )
        )
    );

    let report = regenerate(&prepared.store, "run-1", false)
        .expect("regenerate from authoritative artifacts without executors");
    assert_eq!(
        report.definition_snapshot_sha256,
        manifest.definition_snapshot.sha256
    );
    assert_eq!(
        report.design_counts.test_combinations,
        prepared.expanded.estimates.cell_count
    );
    assert_eq!(
        report.design_counts.trial_batches,
        prepared.expanded.estimates.trial_batch_count
    );
    assert_eq!(
        report.design_counts.issued_product_requests,
        prepared.expanded.estimates.issued_operation_request_count
    );
    assert_eq!(report.methods.schema_version, 1);
    assert_eq!(
        report.methods.plan_schema_version,
        prepared.expanded.schema_version
    );
    assert_eq!(
        report.methods.plan_seed,
        prepared.expanded.canonical_plan.seed
    );
    assert_eq!(
        report.methods.resource_sample_interval_ms,
        prepared
            .expanded
            .canonical_plan
            .protocol
            .resource_interval_ms
    );
    assert_eq!(report.methods.fixture_hashes, manifest.fixture_hashes);
    assert_eq!(report.methods.producer, manifest.producer);
    assert_eq!(report.methods.artifact_schemas, manifest.artifact_schemas);
    assert_eq!(
        report.methods.operation_authorities,
        manifest.operation_authorities
    );
    assert_eq!(report.methods.metric_revisions, manifest.metric_revisions);
    assert_eq!(
        report
            .methods
            .derived_metric_revisions
            .iter()
            .map(|identity| identity.metric_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            PRIMARY_LATENCY_METRIC_ID,
            REQUEST_LATENCY_METRIC_ID,
            THROUGHPUT_METRIC_ID,
            SETUP_METRIC_ID,
            VERIFY_METRIC_ID,
            TEARDOWN_METRIC_ID,
        ]
    );
    assert_eq!(report.methods.check_revisions, manifest.check_revisions);
    assert_eq!(report.methods.phase_revisions, manifest.phase_revisions);
    assert_eq!(report.methods.environment, manifest.environment);
}

#[test]
fn report_rejects_manifest_authority_drift() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    let mut manifest: RunManifest = prepared
        .store
        .read_envelope(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read authoritative manifest");
    manifest.operation_authorities[0].factor_schema_revision += 1;
    prepared
        .store
        .replace_snapshot(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        )
        .expect("persist drifted manifest");

    assert!(matches!(
        regenerate(&prepared.store, "run-1", false),
        Err(ReportError::InvalidManifestAuthority(message))
            if message.contains("operation authority")
    ));
}

#[test]
fn run_manifest_v1_rejects_unknown_fields_strictly() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    let mut manifest: Value = prepared
        .store
        .read_envelope(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        )
        .expect("read manifest JSON");
    manifest["future_authority"] = json!(true);
    prepared
        .store
        .replace_snapshot(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &manifest,
        )
        .expect("persist unknown manifest field");

    assert!(matches!(
        prepared.store.read_envelope::<RunManifest>(
            "run-1",
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
        ),
        Err(ArtifactError::Json { .. })
    ));
}

#[test]
fn current_snapshot_regenerates_and_missing_registered_metrics_are_unavailable() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    let cell = &prepared.expanded.cells[0];
    prepared
        .store
        .append_record(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
            &SequencedObservation {
                sequence: 1,
                record: ObservationRecord::Trial(TrialSample {
                    operation_id: cell.operation_id,
                    cell_id: cell.cell_id.clone(),
                    trial_id: "trial-1".to_owned(),
                    kind: TrialKind::Measured,
                    sequence_in_cell: 1,
                    lifecycle: LifecycleDurations {
                        setup_ns: 1,
                        operation_ns: 10,
                        verify_ns: 1,
                        teardown_ns: 1,
                    },
                    product_succeeded: true,
                    infrastructure_failed: false,
                    cleanup_baseline_restored: true,
                    correctness: CorrectnessFold {
                        product_succeeded: true,
                        required_check_count: 0,
                        attempted_check_count: 0,
                        passed_check_count: 0,
                        failed_check_count: 0,
                        missing_checks: Vec::new(),
                        unexpected_checks: Vec::new(),
                        eligible_for_latency: true,
                    },
                    primary_operation_latency_ns: Some(10),
                    artifacts: Vec::new(),
                }),
            },
        )
        .expect("write trial observation");

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate report");
    let cell_report = report
        .cells
        .iter()
        .find(|summary| summary.cell_id == cell.cell_id)
        .expect("reported cell");
    for definition in METRICS {
        let metric = cell_report
            .metrics
            .iter()
            .find(|summary| summary.identity.id == definition.id)
            .expect("registered metric has a report row");
        assert_eq!(metric.attempted_n, 1);
        assert_eq!(metric.failed_n, 0);
        assert_eq!(metric.available_n, 0);
        assert_eq!(metric.unavailable.count, 1);
        assert_eq!(metric.unavailable.reasons.get("missing_reading"), Some(&1));
        assert_eq!(metric.identity.source, "missing_observation");
    }
    let declared_requests = u64::from(cell.operation.measured_invocation_count());
    let request_latency = cell_report
        .metrics
        .iter()
        .find(|metric| metric.identity.id == REQUEST_LATENCY_METRIC_ID)
        .expect("request latency row");
    assert_eq!(request_latency.attempted_n, declared_requests);
    assert_eq!(request_latency.available_n, 0);
    assert_eq!(request_latency.failed_n, 0);
    assert_eq!(request_latency.unavailable.count, declared_requests);
    assert_eq!(
        request_latency
            .unavailable
            .reasons
            .get("missing_request_observation"),
        Some(&declared_requests)
    );
    let throughput = cell_report
        .metrics
        .iter()
        .find(|metric| metric.identity.id == THROUGHPUT_METRIC_ID)
        .expect("throughput row");
    assert_eq!(throughput.attempted_n, 1);
    assert_eq!(throughput.available_n, 0);
    assert_eq!(throughput.unavailable.count, 1);
}

#[test]
fn cpu_latency_correlation_uses_registered_cpu_identity_and_trial_alignment() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    let cell = &prepared.expanded.cells[0];
    let cpu = METRICS
        .iter()
        .find(|metric| metric.id == "sandbox_cpu_time_ns")
        .expect("registered sandbox CPU metric");
    let mut sequence = 0_u64;
    for (index, (latency_ns, cpu_delta_ns)) in [(10_u64, 10_u64), (20, 30), (30, 50)]
        .into_iter()
        .enumerate()
    {
        let trial_id = format!("trial-cpu-{}", index + 1);
        for (offset_ns, value) in [(1_u64, 100_u64), (5, 100 + cpu_delta_ns)] {
            sequence += 1;
            append_observation(
                &prepared,
                sequence,
                ObservationRecord::Resource(ResourceObservation {
                    cell_id: cell.cell_id.clone(),
                    trial_id: trial_id.clone(),
                    request_id: None,
                    reading: ResourceReading {
                        schema_version: 1,
                        metric_id: cpu.id.to_owned(),
                        metric_semantic_revision: cpu.semantic_revision,
                        unit: cpu.unit,
                        scope: cpu.scope,
                        kind: cpu.kind,
                        aggregation: cpu.aggregation,
                        source: "docker_cgroup_v2".to_owned(),
                        monotonic_offset_ns: offset_ns,
                        sampled: false,
                        value: available(value as f64),
                    },
                }),
            );
        }
        let mut trial = successful_measured_trial(cell, &trial_id);
        trial.sequence_in_cell = u32::try_from(index + 1).expect("bounded test index");
        trial.lifecycle.operation_ns = latency_ns;
        trial.primary_operation_latency_ns = Some(latency_ns);
        sequence += 1;
        append_observation(&prepared, sequence, ObservationRecord::Trial(trial));
    }

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate correlation");
    let cell_report = report
        .cells
        .iter()
        .find(|summary| summary.cell_id == cell.cell_id)
        .expect("reported CPU cell");
    let correlation = &cell_report.cpu_latency_correlation;
    assert_eq!(correlation.method, CorrelationMethod::Pearson);
    assert_eq!(
        correlation.alignment,
        CorrelationAlignment::EligibleTrialAggregateByTrialId
    );
    assert_eq!(
        correlation.eligibility,
        CorrelationEligibility::MeasuredProductSuccessChecksPassCleanupRestored
    );
    assert_eq!(correlation.latency_metric_id, PRIMARY_LATENCY_METRIC_ID);
    assert_eq!(correlation.cpu_metric_id, "sandbox_cpu_time_ns");
    assert_eq!(correlation.support_count, 3);
    assert_eq!(correlation.points.len(), 3);
    assert!(correlation
        .coefficient
        .is_some_and(|coefficient| (coefficient - 1.0).abs() < 1e-12));
    assert_eq!(correlation.confidence_interval, None);
    assert_eq!(
        correlation.interval_omission,
        Some(PearsonConfidenceIntervalOmission::InsufficientN)
    );
    assert_eq!(correlation.exclusions.ineligible_trial, 0);
    assert_eq!(correlation.exclusions.missing_latency, 0);
    assert_eq!(correlation.exclusions.missing_cpu, 0);
    assert_eq!(correlation.exclusions.unavailable_cpu, 0);

    let cpu_summary = cell_report
        .metrics
        .iter()
        .find(|metric| {
            metric.identity.id == "sandbox_cpu_time_ns"
                && metric.identity.source == "docker_cgroup_v2"
        })
        .expect("CPU metric summary");
    assert_eq!(cpu_summary.available_n, 3);
    assert_eq!(
        cpu_summary
            .raw_points
            .iter()
            .map(|point| point.raw_integer_value)
            .collect::<Vec<_>>(),
        vec![Some(10), Some(30), Some(50)]
    );
}

#[test]
fn factor_studies_cover_each_metric_and_compare_every_candidate_to_declared_controls() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    for (index, cell) in prepared.expanded.cells.iter().enumerate() {
        let mut trial = successful_measured_trial(cell, &format!("trial-factor-{}", index + 1));
        let latency_ns = 100_u64 + u64::try_from(index).expect("bounded test index");
        trial.lifecycle.operation_ns = latency_ns;
        trial.primary_operation_latency_ns = Some(latency_ns);
        append_observation(
            &prepared,
            u64::try_from(index + 1).expect("bounded test sequence"),
            ObservationRecord::Trial(trial),
        );
    }

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate factor studies");
    let operation_id = prepared.expanded.cells[0].operation_id;
    let studies = report
        .factor_studies
        .iter()
        .filter(|study| study.operation_id == operation_id)
        .collect::<Vec<_>>();
    assert!(
        studies.len() >= 6,
        "all timing metrics have factor projections"
    );
    let latency = studies
        .iter()
        .copied()
        .find(|study| study.metric.id == PRIMARY_LATENCY_METRIC_ID)
        .expect("batch-makespan factor study");
    assert_eq!(latency.cells.len(), prepared.expanded.cells.len());
    assert_eq!(
        latency.control_comparisons.len(),
        prepared.expanded.cells.len() - 1
    );
    let control_cell_id = latency.control_comparisons[0].control_cell_id.clone();
    let control = latency
        .cells
        .iter()
        .find(|cell| cell.cell_id == control_cell_id)
        .expect("declared all-control cell");
    assert!(control.factors.iter().all(|factor| match factor.role {
        sandbox_benchmark::model::FactorRole::Controlled => true,
        sandbox_benchmark::model::FactorRole::Varied => {
            factor.control.as_ref() == Some(&factor.value)
        }
    }));
    for comparison in &latency.control_comparisons {
        assert_eq!(comparison.control_cell_id, control_cell_id);
        assert!(!comparison.changed_factor_ids.is_empty());
        assert_eq!(
            comparison.absolute_difference,
            comparison
                .control_median
                .zip(comparison.candidate_median)
                .map(|(control, candidate)| candidate - control)
        );
        assert!(comparison.percentage_difference.is_some());
        assert_eq!(
            comparison.interval_omission_reason.as_deref(),
            Some("insufficient_n")
        );
    }
}

#[test]
fn report_projects_historical_factors_timelines_and_bounded_check_evidence() {
    let prepared = prepare_run_for_scope(
        serde_json::to_value(catalog()).expect("serialize catalog"),
        ConfigurationScope::LayerStack,
    );
    let cell = &prepared.expanded.cells[0];
    let operation = definition(cell.operation_id);
    let phase = operation.phases.first().expect("registered phase");
    let check = operation.checks.first().expect("registered check");
    let metric = &METRICS[0];
    let trial_id = "trial-projection-1";
    let request_id = "request-projection-1";

    append_observation(
        &prepared,
        1,
        ObservationRecord::Request(request_observation(cell, trial_id, request_id)),
    );
    append_observation(
        &prepared,
        2,
        ObservationRecord::Phase(PhaseObservation {
            id: phase.id,
            semantic_revision: phase.semantic_revision,
            unit: phase.unit,
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            source: phase.source,
            correlation: phase.correlation,
            trace_span_name: phase.trace_span_name.to_owned(),
            start_offset_ns: 6,
            duration_ns: 5,
            status: PhaseStatus::Succeeded,
        }),
    );
    append_observation(
        &prepared,
        3,
        ObservationRecord::Resource(ResourceObservation {
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            reading: ResourceReading {
                schema_version: 1,
                metric_id: metric.id.to_owned(),
                metric_semantic_revision: metric.semantic_revision,
                unit: metric.unit,
                scope: metric.scope,
                kind: metric.kind,
                aggregation: metric.aggregation,
                source: "projection_test".to_owned(),
                monotonic_offset_ns: 8,
                sampled: true,
                value: Availability::Unavailable {
                    source: "projection_test".to_owned(),
                    reason: "counter_unavailable".to_owned(),
                },
            },
        }),
    );
    append_observation(
        &prepared,
        4,
        ObservationRecord::Check(CheckResult {
            id: check.id,
            semantic_revision: check.semantic_revision,
            operation_id: cell.operation_id,
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            verdict: CheckVerdict::Pass,
            duration_ns: 3,
            evidence: bounded_evidence(
                check.id,
                vec![CheckEvidenceItem {
                    expected: "expected projection".to_owned(),
                    actual: "actual projection".to_owned(),
                    artifact_id: Some("bounded-evidence.json".to_owned()),
                }],
            ),
        }),
    );
    let mut trial = successful_measured_trial(cell, trial_id);
    trial.correctness = CorrectnessFold {
        product_succeeded: true,
        required_check_count: 1,
        attempted_check_count: 1,
        passed_check_count: 1,
        failed_check_count: 0,
        missing_checks: Vec::new(),
        unexpected_checks: Vec::new(),
        eligible_for_latency: true,
    };
    append_observation(&prepared, 5, ObservationRecord::Trial(trial));

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate projections");
    let cell_report = report
        .cells
        .iter()
        .find(|summary| summary.cell_id == cell.cell_id)
        .expect("projected cell");
    assert_eq!(cell_report.factors.len(), operation.factors.len());
    assert!(cell_report.factors.iter().all(|factor| {
        !factor.label.is_empty()
            && !factor.help.is_empty()
            && matches!(
                factor.value,
                ReportFactorValue::UnsignedInteger(_)
                    | ReportFactorValue::Ratio(_)
                    | ReportFactorValue::Choice(_)
            )
    }));

    let study = report
        .factor_studies
        .iter()
        .find(|study| {
            study.operation_id == cell.operation_id && study.metric.id == PRIMARY_LATENCY_METRIC_ID
        })
        .expect("operation factor study");
    assert!(!matches!(study.layout, FactorStudyLayout::SingleCell));
    let study_cell = study
        .cells
        .iter()
        .find(|projection| projection.cell_id == cell.cell_id)
        .expect("factor-study cell");
    assert_eq!(study_cell.raw_points.len(), 1);
    assert_eq!(study_cell.raw_points[0].trial_id, trial_id);
    assert_eq!(study_cell.raw_points[0].value, 10.0);
    assert_eq!(study_cell.raw_points[0].raw_integer_value, Some(10));
    for metric_id in [
        PRIMARY_LATENCY_METRIC_ID,
        REQUEST_LATENCY_METRIC_ID,
        THROUGHPUT_METRIC_ID,
        SETUP_METRIC_ID,
        VERIFY_METRIC_ID,
        TEARDOWN_METRIC_ID,
    ] {
        assert!(report.factor_studies.iter().any(|study| {
            study.operation_id == cell.operation_id && study.metric.id == metric_id
        }));
    }
    let request_latency = cell_report
        .metrics
        .iter()
        .find(|metric| metric.identity.id == REQUEST_LATENCY_METRIC_ID)
        .expect("request latency metric");
    assert_eq!(request_latency.attempted_n, 1);
    assert_eq!(request_latency.available_n, 1);
    assert_eq!(request_latency.raw_points[0].trial_id, trial_id);
    assert_eq!(
        request_latency.raw_points[0].request_id.as_deref(),
        Some(request_id)
    );
    let throughput = cell_report
        .metrics
        .iter()
        .find(|metric| metric.identity.id == THROUGHPUT_METRIC_ID)
        .expect("throughput metric");
    assert_eq!(throughput.identity.unit, MetricUnit::OperationsPerSecond);
    assert_eq!(throughput.statistics.median, Some(100_000_000.0));

    assert_eq!(cell_report.timelines.len(), 1);
    let timeline = &cell_report.timelines[0];
    assert_eq!(timeline.trial_id, trial_id);
    assert_eq!(timeline.domain_start_ns, 5);
    assert_eq!(timeline.domain_end_ns, 15);
    assert_eq!(
        timeline.operation_window,
        Some(sandbox_benchmark::report::OperationWindowProjection {
            start_offset_ns: 5,
            duration_ns: 10,
        })
    );
    assert_eq!(timeline.request_spans.len(), 1);
    assert_eq!(timeline.phase_spans.len(), 1);
    assert_eq!(timeline.phase_spans[0].label, phase.label);
    assert_eq!(timeline.series.len(), 1);
    assert!(matches!(
        timeline.series[0].points[0].value,
        Availability::Unavailable { ref reason, .. } if reason == "counter_unavailable"
    ));

    assert_eq!(cell_report.check_evidence.len(), 1);
    let evidence = &cell_report.check_evidence[0];
    assert_eq!(evidence.label, check.label);
    assert_eq!(evidence.help, check.help);
    assert_eq!(evidence.evidence.items[0].expected, "expected projection");
    assert_eq!(evidence.evidence.items[0].actual, "actual projection");
    assert_eq!(
        evidence.evidence.items[0].artifact_id.as_deref(),
        Some("bounded-evidence.json")
    );

    assert_eq!(
        regenerate(&prepared.store, "run-1", false).expect("regenerate without executors"),
        report
    );
}

#[test]
fn json_and_csv_exports_regenerate_deterministically_from_authoritative_artifacts() {
    let prepared = prepare_run(serde_json::to_value(catalog()).expect("serialize catalog"));
    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate report");
    let exported: BenchmarkReport = prepared
        .store
        .read_envelope(
            "run-1",
            ArtifactId::JsonExport,
            JSON_EXPORT_SCHEMA_NAME,
            JSON_EXPORT_SCHEMA_VERSION,
        )
        .expect("read versioned JSON export");
    assert_eq!(exported, report);

    let csv_before = prepared
        .store
        .content("run-1", "csv_export")
        .expect("read CSV export")
        .bytes;
    let csv_text = std::str::from_utf8(&csv_before).expect("CSV is UTF-8");
    assert!(csv_text.starts_with("export_schema_version,run_id,plan_hash,record_type,"));
    assert!(csv_text.contains("metric_summary"));
    let version_prefix = format!("{CSV_EXPORT_SCHEMA_VERSION},");
    assert!(csv_text
        .lines()
        .skip(1)
        .all(|line| line.starts_with(&version_prefix)));

    regenerate(&prepared.store, "run-1", false).expect("regenerate exports again");
    assert_eq!(
        prepared
            .store
            .content("run-1", "csv_export")
            .expect("read regenerated CSV export")
            .bytes,
        csv_before
    );
}

#[test]
fn unknown_nested_definition_snapshot_fields_are_rejected() {
    let mut snapshot = serde_json::to_value(catalog()).expect("serialize catalog");
    snapshot["operations"][0]["future_field"] = json!(true);
    let prepared = prepare_run(snapshot);

    assert!(matches!(
        regenerate(&prepared.store, "run-1", false),
        Err(ReportError::Artifact(ArtifactError::Json { .. }))
    ));
}

#[test]
fn duplicate_closed_operation_identities_are_rejected() {
    let mut snapshot = serde_json::to_value(catalog()).expect("serialize catalog");
    snapshot["operations"][1]["id"] = snapshot["operations"][0]["id"].clone();
    let prepared = prepare_run(snapshot);

    assert!(matches!(
        regenerate(&prepared.store, "run-1", false),
        Err(ReportError::InvalidDefinitionSnapshot(message))
            if message.contains("operation ids must be unique")
    ));
}

#[test]
fn layerstack_evidence_regenerates_from_authoritative_artifacts_with_typed_outcomes() {
    let prepared = prepare_run_for_scope(
        serde_json::to_value(catalog()).expect("serialize catalog"),
        ConfigurationScope::LayerStack,
    );
    let cell = &prepared.expanded.cells[0];
    let trial_id = "trial-layerstack-1";
    let request_id = "request-layerstack-1";
    let mut sampled_peak = storage_snapshot(20, 20_480);
    sampled_peak.storage_allocated_bytes = Availability::Unavailable {
        source: "docker_stats".to_owned(),
        reason: "counter_unavailable".to_owned(),
    };
    let evidence = SquashLayerstackEvidence {
        requested_live_sessions: 4,
        observed_migrated_sessions: 3,
        observed_non_migrated_sessions: 1,
        dispositions: SessionDispositionCounts {
            migrated: 3,
            identity: 1,
            leased: 0,
            faulty: 0,
            session_gone: 0,
        },
        effective_remount_parallelism: 2,
        observed_squashed_block_count: 2,
        observed_replaced_layer_count: 4,
        source_layer_ids: vec![
            "layer-1".to_owned(),
            "layer-2".to_owned(),
            "layer-3".to_owned(),
            "layer-4".to_owned(),
        ],
        retained_source_layer_ids: vec!["layer-4".to_owned()],
        source_layer_allocations: vec![
            SourceLayerAllocation {
                layer_id: "layer-1".to_owned(),
                logical_bytes: available(1_024),
                allocated_bytes: available(4_096),
            },
            SourceLayerAllocation {
                layer_id: "layer-2".to_owned(),
                logical_bytes: available(1_024),
                allocated_bytes: Availability::Unavailable {
                    source: "filesystem".to_owned(),
                    reason: "not_supported".to_owned(),
                },
            },
        ],
        reclaimed_bytes: available(8_192),
        s0_baseline: storage_snapshot(10, 16_384),
        s1_sampled_peak: sampled_peak,
        s2_post_commit: storage_snapshot(30, 12_288),
        s3_settled: storage_snapshot(40, 8_192),
        manifest_reduced: true,
        content_equivalent: true,
        usable_session_count: 4,
    };
    let typed_evidence = OperationEvidence::SquashLayerstack(Box::new(evidence.clone()));
    let evidence_reference = prepared
        .store
        .write_trial_evidence("run-1", &cell.cell_id, trial_id, &typed_evidence)
        .expect("write immutable typed operation evidence");

    append_observation(
        &prepared,
        1,
        ObservationRecord::Request(request_observation(cell, trial_id, request_id)),
    );
    append_observation(
        &prepared,
        2,
        ObservationRecord::Operation(OperationObservation {
            operation_id: cell.operation_id,
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            evidence: typed_evidence,
        }),
    );
    let mut trial = successful_measured_trial(cell, trial_id);
    trial.artifacts.push(evidence_reference.clone());
    append_observation(&prepared, 3, ObservationRecord::Trial(trial));

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate typed report");
    let cell_report = report
        .cells
        .iter()
        .find(|summary| summary.cell_id == cell.cell_id)
        .expect("layerstack cell report");
    assert_eq!(cell_report.operation_evidence.len(), 1);
    let projected = &cell_report.operation_evidence[0];
    assert_eq!(projected.trial_id, trial_id);
    assert_eq!(projected.request_id.as_deref(), Some(request_id));
    assert_eq!(
        projected.evidence,
        OperationEvidence::SquashLayerstack(Box::new(evidence))
    );
    assert!(prepared
        .store
        .index("run-1")
        .expect("index report artifacts")
        .iter()
        .any(|entry| entry.artifact_id == evidence_reference.artifact_id));

    let serialized = serde_json::to_value(&report).expect("serialize regenerated report");
    let serialized_evidence = &serialized["cells"]
        .as_array()
        .expect("report cells")
        .iter()
        .find(|summary| summary["cell_id"] == cell.cell_id)
        .expect("serialized layerstack cell")["operation_evidence"][0];
    assert_eq!(
        serialized_evidence["evidence"]["operation"],
        "squash_layerstack"
    );
    assert_eq!(
        serialized_evidence["evidence"]["evidence"]["requested_live_sessions"],
        4
    );
    assert_eq!(
        serialized_evidence["evidence"]["evidence"]["s1_sampled_peak"]["storage_allocated_bytes"]
            ["availability"],
        "unavailable"
    );

    assert_eq!(
        regenerate(&prepared.store, "run-1", false).expect("regenerate without executors"),
        report
    );
}

#[test]
fn registered_phase_identity_is_preserved_and_mismatches_are_rejected() {
    let snapshot = serde_json::to_value(catalog()).expect("serialize catalog");
    let prepared = prepare_run_for_scope(snapshot.clone(), ConfigurationScope::LayerStack);
    let cell = &prepared.expanded.cells[0];
    let trial_id = "trial-phase-1";
    let request_id = "request-phase-1";
    let phase = definition(cell.operation_id)
        .phases
        .first()
        .expect("layerstack phase definition");

    append_observation(
        &prepared,
        1,
        ObservationRecord::Request(request_observation(cell, trial_id, request_id)),
    );
    append_observation(
        &prepared,
        2,
        ObservationRecord::Phase(PhaseObservation {
            id: phase.id,
            semantic_revision: phase.semantic_revision,
            unit: phase.unit,
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            source: phase.source,
            correlation: phase.correlation,
            trace_span_name: phase.trace_span_name.to_owned(),
            start_offset_ns: 6,
            duration_ns: 9,
            status: PhaseStatus::Succeeded,
        }),
    );
    append_observation(
        &prepared,
        3,
        ObservationRecord::Trial(successful_measured_trial(cell, trial_id)),
    );

    let report = regenerate(&prepared.store, "run-1", false).expect("regenerate phase report");
    let summary = report
        .cells
        .iter()
        .find(|summary| summary.cell_id == cell.cell_id)
        .and_then(|summary| summary.phases.first())
        .expect("phase summary");
    assert_eq!(summary.id, phase.id);
    assert_eq!(summary.semantic_revision, phase.semantic_revision);
    assert_eq!(summary.unit, phase.unit);
    assert_eq!(summary.source, phase.source);
    assert_eq!(summary.correlation, phase.correlation);
    assert_eq!(summary.trace_span_name, phase.trace_span_name);
    assert_eq!(summary.duration.median, Some(9.0));

    let mismatched = prepare_run_for_scope(snapshot, ConfigurationScope::LayerStack);
    let mismatched_cell = &mismatched.expanded.cells[0];
    append_observation(
        &mismatched,
        1,
        ObservationRecord::Request(request_observation(mismatched_cell, trial_id, request_id)),
    );
    append_observation(
        &mismatched,
        2,
        ObservationRecord::Phase(PhaseObservation {
            id: phase.id,
            semantic_revision: phase.semantic_revision,
            unit: phase.unit,
            cell_id: mismatched_cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            source: phase.source,
            correlation: phase.correlation,
            trace_span_name: "layerstack.squash.unregistered".to_owned(),
            start_offset_ns: 6,
            duration_ns: 9,
            status: PhaseStatus::Succeeded,
        }),
    );
    append_observation(
        &mismatched,
        3,
        ObservationRecord::Trial(successful_measured_trial(mismatched_cell, trial_id)),
    );
    assert!(matches!(
        regenerate(&mismatched.store, "run-1", false),
        Err(ReportError::InvalidDefinitionSnapshot(message))
            if message.contains("does not match its operation definition snapshot")
    ));
}
