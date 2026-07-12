use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::artifacts::{
    ArtifactError, ArtifactId, ArtifactStore, SchemaEnvelope, BOUNDED_EVIDENCE_SCHEMA_NAME,
    BOUNDED_EVIDENCE_SCHEMA_VERSION,
};
use crate::checks::{BoundedCheckEvidence, CheckVerdict, CorrectnessFold};
use crate::definitions::OperationComparisonKey;
use crate::events::RunState;
use crate::fixtures::WorkspaceProfileCatalog;
use crate::model::{
    CheckId, CleanupPolicy, ClientCohort, CountSemantics, ExecutionShape, FactorId, FactorRole,
    FamilyId, IsolationPolicy, OperationEvidence, OperationId, OperationPlan, PhaseCorrelationRule,
    PhaseId, PhaseSource, PhaseUnit, ProductAccess, ProductOperation, ResolvedIsolationPolicy,
    SecurityClass, WorkspaceAction,
};
use crate::plan::{ExpandedCell, ExpandedPlan};
use crate::resources::{
    AggregationRule, Availability, AvailabilityPolicy, MetricDirection, MetricKind, MetricScope,
    MetricUnit, ResourceReading,
};
use crate::scheduler::{
    is_terminal, ArtifactSchemaIdentity, CapResolution, CapUnit, ObservationRecord,
    OperationObservation, PhaseStatus, ProductCapId, RunManifest, SequencedObservation,
    StabilizationPolicy, TrialKind, TrialSample, DEFINITION_SNAPSHOT_SCHEMA_NAME,
    ENVIRONMENT_METADATA_SCHEMA_NAME, EXPANDED_PLAN_SCHEMA_NAME, INTENT_PLAN_SCHEMA_NAME,
    OBSERVATION_SCHEMA_NAME, OBSERVATION_SCHEMA_VERSION, RUN_MANIFEST_SCHEMA_NAME,
    RUN_MANIFEST_SCHEMA_VERSION,
};
use crate::statistics::{self, ConfidenceIntervalOmission, SampleStatistics, StatisticsError};

pub use crate::model::NormalizedFactorValue as ReportFactorValue;

pub const SUMMARY_SCHEMA_NAME: &str = "eos_benchmark_summary";
pub const SUMMARY_SCHEMA_VERSION: u32 = 4;
pub const REPORT_SCHEMA_NAME: &str = "eos_benchmark_report";
pub const REPORT_SCHEMA_VERSION: u32 = 4;
pub const JSON_EXPORT_SCHEMA_NAME: &str = "eos_benchmark_json_export";
pub const JSON_EXPORT_SCHEMA_VERSION: u32 = 4;
pub const CSV_EXPORT_SCHEMA_VERSION: u32 = 3;
pub const REPORT_DERIVATION_REVISION: u32 = 3;
pub const PRIMARY_LATENCY_METRIC_ID: &str = "batch_makespan_ns";
pub const PRIMARY_LATENCY_METRIC_REVISION: u32 = 1;
pub const REQUEST_LATENCY_METRIC_ID: &str = "request_latency_ns";
pub const THROUGHPUT_METRIC_ID: &str = "throughput_ops_s";
pub const SETUP_METRIC_ID: &str = "setup_ns";
pub const VERIFY_METRIC_ID: &str = "verify_ns";
pub const TEARDOWN_METRIC_ID: &str = "teardown_ns";
const UNOBSERVED_RESOURCE_SOURCE: &str = "missing_observation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectnessVerdict {
    Pass,
    Fail,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureCounts {
    pub total_attempted: u64,
    pub warmup: u64,
    pub measured_attempted: u64,
    pub successful: u64,
    pub product_failed: u64,
    pub correctness_failed: u64,
    pub infrastructure_failed: u64,
    pub cleanup_invalid: u64,
    pub missing_primary_latency: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricIdentity {
    pub id: String,
    pub label: String,
    pub help: String,
    pub semantic_revision: u32,
    pub unit: MetricUnit,
    pub scope: MetricScope,
    pub kind: MetricKind,
    pub availability: AvailabilityPolicy,
    pub aggregation: AggregationRule,
    pub direction: MetricDirection,
    pub source: String,
    pub ratio_scale: bool,
    pub report_derivation_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricRawPoint {
    pub trial_id: String,
    pub request_id: Option<String>,
    pub value: f64,
    pub raw_integer_value: Option<u64>,
    pub outlier: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnavailabilitySummary {
    pub count: u64,
    pub reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSummary {
    pub identity: MetricIdentity,
    pub attempted_n: u64,
    pub failed_n: u64,
    pub available_n: u64,
    pub unavailable: UnavailabilitySummary,
    pub statistics: SampleStatistics,
    pub raw_points: Vec<MetricRawPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSummary {
    pub id: CheckId,
    pub label: String,
    pub help: String,
    pub semantic_revision: u32,
    pub attempted: u64,
    pub passed: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseSummary {
    pub id: PhaseId,
    pub label: String,
    pub help: String,
    pub semantic_revision: u32,
    pub unit: PhaseUnit,
    pub source: PhaseSource,
    pub correlation: PhaseCorrelationRule,
    pub trace_span_name: String,
    pub attempted: u64,
    pub failed: u64,
    pub duration: SampleStatistics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorDisplayUnit {
    Count,
    Bytes,
    Ratio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportFactor {
    pub id: FactorId,
    pub label: String,
    pub help: String,
    pub role: FactorRole,
    pub unit: Option<FactorDisplayUnit>,
    pub value: ReportFactorValue,
    pub control: Option<ReportFactorValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorStudyCell {
    pub cell_id: String,
    pub factors: Vec<ReportFactor>,
    pub successful_n: u64,
    pub failed_n: u64,
    pub median: Option<f64>,
    pub confidence_interval: Option<ReportConfidenceInterval>,
    pub interval_omission_reason: Option<String>,
    pub raw_points: Vec<MetricRawPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactorStudyLayout {
    SingleCell,
    Trend {
        factor_id: FactorId,
    },
    Matrix {
        row_factor_id: FactorId,
        column_factor_id: FactorId,
    },
    SmallMultiples {
        factor_ids: Vec<FactorId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorStudyProjection {
    pub operation_id: OperationId,
    pub operation_label: String,
    pub metric: MetricIdentity,
    pub layout: FactorStudyLayout,
    pub varied_factor_ids: Vec<FactorId>,
    pub controlled_factor_ids: Vec<FactorId>,
    pub cells: Vec<FactorStudyCell>,
    pub control_comparisons: Vec<ControlComparisonProjection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlComparisonProjection {
    pub comparison_id: String,
    pub control_cell_id: String,
    pub candidate_cell_id: String,
    pub changed_factor_ids: Vec<FactorId>,
    pub control_median: Option<f64>,
    pub candidate_median: Option<f64>,
    pub absolute_difference: Option<f64>,
    pub percentage_difference: Option<f64>,
    pub median_difference_confidence_interval: Option<ReportConfidenceInterval>,
    pub interval_omission_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpanProjection {
    pub request_id: String,
    pub start_offset_ns: u64,
    pub duration_ns: u64,
    pub succeeded: bool,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseSpanProjection {
    pub id: PhaseId,
    pub label: String,
    pub help: String,
    pub semantic_revision: u32,
    pub request_id: Option<String>,
    pub start_offset_ns: u64,
    pub duration_ns: u64,
    pub status: PhaseStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTimelinePoint {
    pub monotonic_offset_ns: u64,
    pub sampled: bool,
    pub value: Availability<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSeriesProjection {
    pub identity: MetricIdentity,
    pub request_id: Option<String>,
    pub points: Vec<ResourceTimelinePoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTimelineProjection {
    pub trial_id: String,
    pub domain_start_ns: u64,
    pub domain_end_ns: u64,
    pub operation_window: Option<OperationWindowProjection>,
    pub request_spans: Vec<RequestSpanProjection>,
    pub phase_spans: Vec<PhaseSpanProjection>,
    pub series: Vec<ResourceSeriesProjection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationWindowProjection {
    pub start_offset_ns: u64,
    pub duration_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckEvidenceReport {
    pub id: CheckId,
    pub label: String,
    pub help: String,
    pub semantic_revision: u32,
    pub trial_id: String,
    pub request_id: Option<String>,
    pub verdict: CheckVerdict,
    pub duration_ns: u64,
    pub evidence: BoundedCheckEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEvidenceReport {
    pub trial_id: String,
    pub request_id: Option<String>,
    pub evidence: OperationEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrelationPoint {
    pub trial_id: String,
    pub operation_latency_ns: f64,
    pub sandbox_cpu_time_ns: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrelationExclusions {
    pub ineligible_trial: u64,
    pub missing_latency: u64,
    pub missing_cpu: u64,
    pub unavailable_cpu: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuLatencyCorrelation {
    pub semantic_revision: u32,
    pub method: CorrelationMethod,
    pub alignment: CorrelationAlignment,
    pub eligibility: CorrelationEligibility,
    pub latency_metric_id: String,
    pub cpu_metric_id: String,
    pub support_count: u64,
    pub coefficient: Option<f64>,
    pub confidence_interval: Option<statistics::PearsonConfidenceInterval>,
    pub interval_omission: Option<statistics::PearsonConfidenceIntervalOmission>,
    pub points: Vec<CorrelationPoint>,
    pub exclusions: CorrelationExclusions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationMethod {
    Pearson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationAlignment {
    EligibleTrialAggregateByTrialId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationEligibility {
    MeasuredProductSuccessChecksPassCleanupRestored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSummary {
    pub cell_id: String,
    pub family_id: FamilyId,
    pub family_label: String,
    pub operation_id: OperationId,
    pub operation_label: String,
    pub comparison_key: OperationComparisonKey,
    pub design_counts: ReportDesignCounts,
    pub factors: Vec<ReportFactor>,
    pub counts: FailureCounts,
    pub metrics: Vec<MetricSummary>,
    pub checks: Vec<CheckSummary>,
    pub phases: Vec<PhaseSummary>,
    pub timelines: Vec<ResourceTimelineProjection>,
    pub check_evidence: Vec<CheckEvidenceReport>,
    pub operation_evidence: Vec<OperationEvidenceReport>,
    pub cpu_latency_correlation: CpuLatencyCorrelation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDerivedSummary {
    pub schema_version: u32,
    pub report_derivation_revision: u32,
    pub run_id: String,
    pub plan_hash: String,
    pub state: RunState,
    pub provisional: bool,
    pub correctness_verdict: CorrectnessVerdict,
    pub design_counts: ReportDesignCounts,
    pub cells: Vec<CellSummary>,
    pub warnings: Vec<ReportWarning>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportResultRow {
    pub row_id: String,
    pub operation_id: OperationId,
    pub cell_id: String,
    pub metric_id: String,
    pub unit: MetricUnit,
    pub successful_n: u64,
    pub failed_n: u64,
    pub unavailable_n: u64,
    pub median: Option<f64>,
    pub confidence_interval: Option<ReportConfidenceInterval>,
    pub interval_omission_reason: Option<String>,
    pub direction: MetricDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportConfidenceInterval {
    pub level: f64,
    pub lower: f64,
    pub upper: f64,
    pub method: ReportConfidenceMethod,
    pub resamples: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportConfidenceMethod {
    PercentileBootstrapMedian,
    PercentileBootstrapMedianDifference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportDesignCounts {
    pub test_combinations: u64,
    pub trial_batches: u64,
    pub issued_product_requests: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodsReport {
    pub schema_version: u32,
    pub report_derivation_revision: u32,
    pub artifact_reader_revision: u32,
    pub plan_schema_version: u32,
    pub plan_seed: u64,
    pub cell_order: crate::model::CellOrder,
    pub resource_sample_interval_ms: u64,
    pub design_counts: ReportDesignCounts,
    pub fixture_generator_revision: u32,
    pub fixture_hashes: BTreeMap<String, String>,
    pub producer: crate::scheduler::ProducerIdentity,
    pub artifact_schemas: crate::scheduler::ArtifactSchemaSet,
    pub operation_authorities: Vec<crate::scheduler::OperationAuthority>,
    pub metric_revisions: Vec<crate::scheduler::MetricRevisionIdentity>,
    pub derived_metric_revisions: Vec<crate::scheduler::MetricRevisionIdentity>,
    pub check_revisions: Vec<crate::scheduler::CheckRevisionIdentity>,
    pub phase_revisions: Vec<crate::scheduler::PhaseRevisionIdentity>,
    pub environment: crate::scheduler::EnvironmentMetadata,
    pub raw_time_unit: String,
    pub monotonic_clock: String,
    pub quantile_interpolation: String,
    pub confidence_interval: String,
    pub bootstrap_resamples: usize,
    pub outlier_policy: String,
    pub warmup_policy: String,
    pub failure_policy: String,
    pub resource_policy: String,
    pub comparison_policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub report_derivation_revision: u32,
    pub run_id: String,
    pub state: RunState,
    pub provisional: bool,
    pub correctness_verdict: CorrectnessVerdict,
    pub design_counts: ReportDesignCounts,
    pub research_question: String,
    pub plan_hash: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub environment_fingerprint: String,
    pub definition_snapshot_version: u32,
    pub definition_snapshot_sha256: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub summary: Vec<ReportResultRow>,
    pub factor_studies: Vec<FactorStudyProjection>,
    pub cells: Vec<CellSummary>,
    pub methods: MethodsReport,
    pub limitations: Vec<String>,
    pub warnings: Vec<ReportWarning>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMetricDefinition {
    id: String,
    semantic_revision: u32,
    unit: MetricUnit,
    scope: MetricScope,
    kind: MetricKind,
    availability: AvailabilityPolicy,
    aggregation: AggregationRule,
    direction: MetricDirection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotCatalog {
    schema_version: u32,
    families: Vec<SnapshotFamily>,
    factor_roles: Vec<FactorRole>,
    metrics: Vec<SnapshotMetricDefinition>,
    workspace_profiles: WorkspaceProfileCatalog,
    operations: Vec<SnapshotOperation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFamily {
    id: FamilyId,
    label: String,
    help: String,
    research_question: String,
    measured_boundary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotFactorValueKind {
    UnsignedInteger,
    UnitRatio,
    Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotFactorUnit {
    Count,
    Bytes,
    Ratio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotProfileCatalogId {
    WorkspaceProfiles,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SnapshotFactorConstraint {
    Positive,
    NonNegative,
    UnitInterval,
    Choices { values: Vec<String> },
    ProfileCatalog { catalog: SnapshotProfileCatalogId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotComparisonParticipation {
    ScientificInvariant,
    NonScientific,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFactorDefinition {
    id: FactorId,
    label: String,
    help: String,
    value_kind: SnapshotFactorValueKind,
    unit: Option<SnapshotFactorUnit>,
    constraint: SnapshotFactorConstraint,
    comparison: SnapshotComparisonParticipation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotCheckReference {
    id: CheckId,
    label: String,
    help: String,
    semantic_revision: u32,
    evidence_limit: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPhaseReference {
    id: PhaseId,
    label: String,
    help: String,
    semantic_revision: u32,
    unit: PhaseUnit,
    source: PhaseSource,
    correlation: PhaseCorrelationRule,
    trace_span_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotComparisonProjection {
    semantic_revision: u32,
    factors: Vec<FactorId>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotOperation {
    id: OperationId,
    family: FamilyId,
    label: String,
    help: String,
    measured_boundary: String,
    count_semantics_help: String,
    semantic_revision: u32,
    factor_schema_revision: u32,
    count_semantics: CountSemantics,
    execution_shape: ExecutionShape,
    isolation: IsolationPolicy,
    cleanup: CleanupPolicy,
    product_access: ProductAccess,
    supported_cohorts: Vec<ClientCohort>,
    security_class: SecurityClass,
    factors: Vec<SnapshotFactorDefinition>,
    checks: Vec<SnapshotCheckReference>,
    phases: Vec<SnapshotPhaseReference>,
    comparison: SnapshotComparisonProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResourceKey {
    metric_id: String,
    semantic_revision: u32,
    unit: MetricUnit,
    scope: MetricScope,
    kind: MetricKind,
    aggregation: AggregationRule,
    source: String,
}

#[derive(Debug, Clone)]
enum TrialAggregate {
    Available(f64),
    Unavailable(String),
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Statistics(#[from] StatisticsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("run {run_id} has plan hash {manifest_hash}, but expanded plan has {expanded_hash}")]
    PlanHashMismatch {
        run_id: String,
        manifest_hash: String,
        expanded_hash: String,
    },
    #[error("observation sequence mismatch: expected {expected}, received {actual}")]
    ObservationSequence { expected: u64, actual: u64 },
    #[error("definition snapshot is invalid: {0}")]
    InvalidDefinitionSnapshot(String),
    #[error("run manifest authority is inconsistent: {0}")]
    InvalidManifestAuthority(String),
    #[error("run {0} is non-terminal; request a provisional report explicitly")]
    ProvisionalNotAllowed(String),
    #[error("resource metric {0} is absent from the persisted definition snapshot")]
    UnknownPersistedMetric(String),
    #[error("resource metric {0} does not match its persisted definition")]
    MetricDefinitionMismatch(String),
    #[error("trial {trial_id} has invalid artifact reference {artifact_id}: {reason}")]
    InvalidArtifactReference {
        trial_id: String,
        artifact_id: String,
        reason: String,
    },
}

pub fn regenerate(
    store: &ArtifactStore,
    run_id: &str,
    allow_provisional: bool,
) -> Result<BenchmarkReport, ReportError> {
    let manifest: RunManifest = store.read_envelope(
        run_id,
        ArtifactId::RunManifest,
        RUN_MANIFEST_SCHEMA_NAME,
        RUN_MANIFEST_SCHEMA_VERSION,
    )?;
    let expanded: ExpandedPlan = store.read_envelope(
        run_id,
        ArtifactId::ExpandedPlan,
        EXPANDED_PLAN_SCHEMA_NAME,
        crate::plan::EXPANDED_PLAN_SCHEMA_VERSION,
    )?;
    if manifest.plan_hash != expanded.plan_hash {
        return Err(ReportError::PlanHashMismatch {
            run_id: run_id.to_owned(),
            manifest_hash: manifest.plan_hash,
            expanded_hash: expanded.plan_hash,
        });
    }
    let snapshot: SnapshotCatalog = store.read_envelope(
        run_id,
        ArtifactId::DefinitionSnapshot,
        DEFINITION_SNAPSHOT_SCHEMA_NAME,
        manifest.definition_snapshot.schema_version,
    )?;
    validate_snapshot(&snapshot, manifest.definition_snapshot.schema_version)?;
    let definition_snapshot_sha256 = sha256_bytes(
        &store
            .content(run_id, ArtifactId::DefinitionSnapshot.as_str())?
            .bytes,
    );
    validate_manifest_authority(&manifest, &expanded, &snapshot, &definition_snapshot_sha256)?;
    let recovered = store.read_records_recovering::<SequencedObservation>(
        run_id,
        ArtifactId::Observations,
        OBSERVATION_SCHEMA_NAME,
        OBSERVATION_SCHEMA_VERSION,
    )?;
    for (index, observation) in recovered.records.iter().enumerate() {
        let expected = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        if observation.sequence != expected {
            return Err(ReportError::ObservationSequence {
                expected,
                actual: observation.sequence,
            });
        }
    }

    let provisional = !is_terminal(manifest.state);
    if provisional && !allow_provisional {
        return Err(ReportError::ProvisionalNotAllowed(run_id.to_owned()));
    }
    let mut warnings = Vec::new();
    if let Some(partial) = recovered.partial_tail {
        warnings.push(ReportWarning {
            code: "partial_observation_tail".to_owned(),
            message: format!(
                "The incomplete final observation at line {} was quarantined; preceding records remain authoritative.",
                partial.line
            ),
        });
    }
    if provisional {
        warnings.push(ReportWarning {
            code: "provisional_report".to_owned(),
            message: "This report is provisional because the run has not reached a terminal state."
                .to_owned(),
        });
    }

    let metric_definitions = snapshot
        .metrics
        .iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect::<BTreeMap<_, _>>();
    let records = recovered
        .records
        .into_iter()
        .map(|observation| observation.record)
        .collect::<Vec<_>>();
    validate_artifact_references(store, run_id, &records)?;
    validate_observation_links(&expanded, &records, &snapshot)?;
    let mut cells = Vec::with_capacity(expanded.cells.len());
    for cell in &expanded.cells {
        cells.push(summarize_cell(
            cell,
            expanded.canonical_plan.seed,
            &records,
            &snapshot,
            &metric_definitions,
            &expanded.canonical_plan.operations,
        )?);
    }
    let factor_studies = factor_studies(&expanded, &cells)?;
    let design_counts = ReportDesignCounts {
        test_combinations: expanded.estimates.cell_count,
        trial_batches: expanded.estimates.trial_batch_count,
        issued_product_requests: expanded.estimates.issued_operation_request_count,
    };
    let correctness_verdict = if provisional {
        CorrectnessVerdict::Pending
    } else if cells
        .iter()
        .any(|cell| cell.counts.correctness_failed > 0 || cell.counts.cleanup_invalid > 0)
    {
        CorrectnessVerdict::Fail
    } else {
        CorrectnessVerdict::Pass
    };
    let derived = RunDerivedSummary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        report_derivation_revision: REPORT_DERIVATION_REVISION,
        run_id: run_id.to_owned(),
        plan_hash: expanded.plan_hash.clone(),
        state: manifest.state,
        provisional,
        correctness_verdict,
        design_counts,
        cells: cells.clone(),
        warnings: warnings.clone(),
    };
    let summary = report_rows(&cells)?;
    let report = BenchmarkReport {
        schema_version: REPORT_SCHEMA_VERSION,
        report_derivation_revision: REPORT_DERIVATION_REVISION,
        run_id: run_id.to_owned(),
        state: manifest.state,
        provisional,
        correctness_verdict,
        design_counts,
        research_question: research_question(&snapshot, &expanded),
        plan_hash: expanded.plan_hash.clone(),
        source_commit: manifest.treatment.source_commit.clone(),
        source_dirty: manifest.treatment.source_dirty,
        environment_fingerprint: environment_fingerprint(&manifest)?,
        definition_snapshot_version: manifest.definition_snapshot.schema_version,
        definition_snapshot_sha256,
        started_at: manifest.started_at.clone(),
        ended_at: manifest.ended_at.clone(),
        summary,
        factor_studies,
        cells,
        methods: methods(&manifest, &expanded, design_counts),
        limitations: limitations(),
        warnings,
    };
    store.replace_snapshot(
        run_id,
        ArtifactId::Summary,
        SUMMARY_SCHEMA_NAME,
        SUMMARY_SCHEMA_VERSION,
        &derived,
    )?;
    store.replace_snapshot(
        run_id,
        ArtifactId::Report,
        REPORT_SCHEMA_NAME,
        REPORT_SCHEMA_VERSION,
        &report,
    )?;
    store.replace_snapshot(
        run_id,
        ArtifactId::JsonExport,
        JSON_EXPORT_SCHEMA_NAME,
        JSON_EXPORT_SCHEMA_VERSION,
        &report,
    )?;
    let csv = render_csv_export(&report)?;
    store.replace_derived_export(run_id, ArtifactId::CsvExport, csv.as_bytes())?;
    Ok(report)
}

pub fn read_report(store: &ArtifactStore, run_id: &str) -> Result<BenchmarkReport, ReportError> {
    store
        .read_envelope(
            run_id,
            ArtifactId::Report,
            REPORT_SCHEMA_NAME,
            REPORT_SCHEMA_VERSION,
        )
        .map_err(Into::into)
}

fn render_csv_export(report: &BenchmarkReport) -> Result<String, serde_json::Error> {
    const HEADER: [&str; 26] = [
        "export_schema_version",
        "run_id",
        "plan_hash",
        "record_type",
        "operation_id",
        "cell_id",
        "item_id",
        "semantic_revision",
        "unit",
        "scope",
        "source",
        "correlation",
        "trace_span_name",
        "attempted_n",
        "successful_n",
        "failed_n",
        "unavailable_n",
        "median",
        "p95",
        "ci_lower",
        "ci_upper",
        "trial_id",
        "value",
        "related_value",
        "verdict",
        "detail",
    ];

    let mut output = String::new();
    push_csv_row(&mut output, HEADER);
    for cell in &report.cells {
        let operation_id = serialized_scalar(&cell.operation_id)?;
        for metric in &cell.metrics {
            let interval = metric.statistics.median_confidence_interval;
            push_csv_row(
                &mut output,
                [
                    CSV_EXPORT_SCHEMA_VERSION.to_string(),
                    report.run_id.clone(),
                    report.plan_hash.clone(),
                    "metric_summary".to_owned(),
                    operation_id.clone(),
                    cell.cell_id.clone(),
                    metric.identity.id.clone(),
                    metric.identity.semantic_revision.to_string(),
                    serialized_scalar(&metric.identity.unit)?,
                    serialized_scalar(&metric.identity.scope)?,
                    metric.identity.source.clone(),
                    String::new(),
                    String::new(),
                    metric.attempted_n.to_string(),
                    metric.available_n.to_string(),
                    metric.failed_n.to_string(),
                    metric.unavailable.count.to_string(),
                    optional_number(metric.statistics.median),
                    optional_number(metric.statistics.p95),
                    optional_number(interval.map(|value| value.lower)),
                    optional_number(interval.map(|value| value.upper)),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    serialized_json(&metric.unavailable.reasons)?,
                ],
            );
        }
        for check in &cell.checks {
            push_csv_row(
                &mut output,
                [
                    CSV_EXPORT_SCHEMA_VERSION.to_string(),
                    report.run_id.clone(),
                    report.plan_hash.clone(),
                    "check_summary".to_owned(),
                    operation_id.clone(),
                    cell.cell_id.clone(),
                    serialized_scalar(&check.id)?,
                    check.semantic_revision.to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    check.attempted.to_string(),
                    check.passed.to_string(),
                    check.failed.to_string(),
                    "0".to_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    if check.failed == 0 { "pass" } else { "fail" }.to_owned(),
                    String::new(),
                ],
            );
        }
        for phase in &cell.phases {
            push_csv_row(
                &mut output,
                [
                    CSV_EXPORT_SCHEMA_VERSION.to_string(),
                    report.run_id.clone(),
                    report.plan_hash.clone(),
                    "phase_summary".to_owned(),
                    operation_id.clone(),
                    cell.cell_id.clone(),
                    serialized_scalar(&phase.id)?,
                    phase.semantic_revision.to_string(),
                    serialized_scalar(&phase.unit)?,
                    "operation".to_owned(),
                    serialized_scalar(&phase.source)?,
                    serialized_scalar(&phase.correlation)?,
                    phase.trace_span_name.clone(),
                    phase.attempted.to_string(),
                    phase.duration.count.to_string(),
                    phase.failed.to_string(),
                    "0".to_owned(),
                    optional_number(phase.duration.median),
                    optional_number(phase.duration.p95),
                    optional_number(
                        phase
                            .duration
                            .median_confidence_interval
                            .map(|value| value.lower),
                    ),
                    optional_number(
                        phase
                            .duration
                            .median_confidence_interval
                            .map(|value| value.upper),
                    ),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ],
            );
        }
        for evidence in &cell.operation_evidence {
            push_csv_row(
                &mut output,
                [
                    CSV_EXPORT_SCHEMA_VERSION.to_string(),
                    report.run_id.clone(),
                    report.plan_hash.clone(),
                    "operation_evidence".to_owned(),
                    operation_id.clone(),
                    cell.cell_id.clone(),
                    serialized_scalar(&evidence.evidence.id())?,
                    cell.comparison_key.semantic_revision.to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    "1".to_owned(),
                    "1".to_owned(),
                    "0".to_owned(),
                    "0".to_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    evidence.trial_id.clone(),
                    String::new(),
                    evidence.request_id.clone().unwrap_or_default(),
                    String::new(),
                    serialized_json(&evidence.evidence)?,
                ],
            );
        }
        for point in &cell.cpu_latency_correlation.points {
            push_csv_row(
                &mut output,
                [
                    CSV_EXPORT_SCHEMA_VERSION.to_string(),
                    report.run_id.clone(),
                    report.plan_hash.clone(),
                    "correlation_point".to_owned(),
                    operation_id.clone(),
                    cell.cell_id.clone(),
                    "sandbox_cpu_time_vs_operation_latency".to_owned(),
                    PRIMARY_LATENCY_METRIC_REVISION.to_string(),
                    "nanoseconds".to_owned(),
                    "trial".to_owned(),
                    serialized_scalar(&cell.cpu_latency_correlation.method)?,
                    String::new(),
                    String::new(),
                    cell.cpu_latency_correlation.support_count.to_string(),
                    cell.cpu_latency_correlation.support_count.to_string(),
                    "0".to_owned(),
                    "0".to_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    point.trial_id.clone(),
                    point.operation_latency_ns.to_string(),
                    point.sandbox_cpu_time_ns.to_string(),
                    String::new(),
                    String::new(),
                ],
            );
        }
    }
    Ok(output)
}

fn serialized_scalar<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        value => Ok(value.to_string()),
    }
}

fn serialized_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

fn optional_number(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn push_csv_row<I, S>(output: &mut String, fields: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut first = true;
    for field in fields {
        if !first {
            output.push(',');
        }
        first = false;
        let field = field.as_ref();
        if field
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
        {
            output.push('"');
            output.push_str(&field.replace('"', "\"\""));
            output.push('"');
        } else {
            output.push_str(field);
        }
    }
    output.push('\n');
}

fn summarize_cell(
    cell: &ExpandedCell,
    run_seed: u64,
    records: &[ObservationRecord],
    snapshot: &SnapshotCatalog,
    metric_definitions: &BTreeMap<String, &SnapshotMetricDefinition>,
    plan_operations: &[OperationPlan],
) -> Result<CellSummary, ReportError> {
    let trials = records
        .iter()
        .filter_map(|record| match record {
            ObservationRecord::Trial(trial) if trial.cell_id == cell.cell_id => Some(trial),
            _ => None,
        })
        .collect::<Vec<_>>();
    let trial_kinds = trials
        .iter()
        .map(|trial| (trial.trial_id.as_str(), trial.kind))
        .collect::<BTreeMap<_, _>>();
    let measured = trials
        .iter()
        .copied()
        .filter(|trial| trial.kind == TrialKind::Measured)
        .collect::<Vec<_>>();
    let eligible = measured
        .iter()
        .copied()
        .filter(|trial| trial_is_eligible(trial))
        .map(|trial| trial.trial_id.as_str())
        .collect::<BTreeSet<_>>();
    let counts = failure_counts(&trials);
    let mut metrics = timing_summaries(cell, run_seed, records, &measured, &counts)?;

    let mut grouped = BTreeMap::<ResourceKey, BTreeMap<&str, Vec<&ResourceReading>>>::new();
    for record in records {
        let ObservationRecord::Resource(observation) = record else {
            continue;
        };
        if observation.cell_id != cell.cell_id
            || trial_kinds.get(observation.trial_id.as_str()) != Some(&TrialKind::Measured)
        {
            continue;
        }
        let reading = &observation.reading;
        let definition = metric_definitions
            .get(&reading.metric_id)
            .ok_or_else(|| ReportError::UnknownPersistedMetric(reading.metric_id.clone()))?;
        if reading.metric_semantic_revision != definition.semantic_revision
            || reading.unit != definition.unit
            || reading.scope != definition.scope
            || reading.kind != definition.kind
            || reading.aggregation != definition.aggregation
        {
            return Err(ReportError::MetricDefinitionMismatch(
                reading.metric_id.clone(),
            ));
        }
        let key = ResourceKey {
            metric_id: reading.metric_id.clone(),
            semantic_revision: reading.metric_semantic_revision,
            unit: reading.unit,
            scope: reading.scope,
            kind: reading.kind,
            aggregation: reading.aggregation,
            source: reading.source.clone(),
        };
        grouped
            .entry(key)
            .or_default()
            .entry(observation.trial_id.as_str())
            .or_default()
            .push(reading);
    }
    for definition in metric_definitions.values() {
        if grouped.keys().any(|key| key.metric_id == definition.id) {
            continue;
        }
        grouped.insert(
            ResourceKey {
                metric_id: definition.id.clone(),
                semantic_revision: definition.semantic_revision,
                unit: definition.unit,
                scope: definition.scope,
                kind: definition.kind,
                aggregation: definition.aggregation,
                source: UNOBSERVED_RESOURCE_SOURCE.to_owned(),
            },
            BTreeMap::new(),
        );
    }
    let mut cpu_by_trial = BTreeMap::<String, TrialAggregate>::new();
    for (key, by_trial) in grouped {
        let definition = metric_definitions
            .get(&key.metric_id)
            .ok_or_else(|| ReportError::UnknownPersistedMetric(key.metric_id.clone()))?;
        let mut points = Vec::new();
        let mut reasons = BTreeMap::<String, u64>::new();
        for trial in &measured {
            if !eligible.contains(trial.trial_id.as_str()) {
                continue;
            }
            let aggregate = by_trial.get(trial.trial_id.as_str()).map_or_else(
                || TrialAggregate::Unavailable("missing_reading".to_owned()),
                |readings| aggregate_resource(readings, key.aggregation),
            );
            if key.metric_id == "sandbox_cpu_time_ns" {
                cpu_by_trial.insert(trial.trial_id.clone(), aggregate.clone());
            }
            match aggregate {
                TrialAggregate::Available(value) => points.push(MetricPointInput {
                    trial_id: trial.trial_id.clone(),
                    request_id: None,
                    value,
                    raw_integer_value: exact_nonnegative_integer(value),
                }),
                TrialAggregate::Unavailable(reason) => {
                    *reasons.entry(reason).or_default() += 1;
                }
            }
        }
        let seed = bootstrap_seed(run_seed, &cell.cell_id, &key.metric_id);
        let values = points.iter().map(|point| point.value).collect::<Vec<_>>();
        let statistics = statistics::summarize(&values, seed)?;
        let outliers = statistics
            .outlier_indices
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let raw_points = points
            .into_iter()
            .enumerate()
            .map(|(index, point)| MetricRawPoint {
                trial_id: point.trial_id,
                request_id: point.request_id,
                value: point.value,
                raw_integer_value: point.raw_integer_value,
                outlier: outliers.contains(&index),
            })
            .collect::<Vec<_>>();
        let (label, help) = metric_copy(&key.metric_id);
        metrics.push(MetricSummary {
            identity: MetricIdentity {
                id: key.metric_id,
                label: label.to_owned(),
                help: help.to_owned(),
                semantic_revision: key.semantic_revision,
                unit: key.unit,
                scope: key.scope,
                kind: key.kind,
                availability: definition.availability,
                aggregation: key.aggregation,
                direction: definition.direction,
                source: key.source,
                ratio_scale: ratio_scale(key.unit),
                report_derivation_revision: REPORT_DERIVATION_REVISION,
            },
            attempted_n: counts.measured_attempted,
            failed_n: counts
                .measured_attempted
                .saturating_sub(u64::try_from(eligible.len()).unwrap_or(u64::MAX)),
            available_n: u64::try_from(raw_points.len()).unwrap_or(u64::MAX),
            unavailable: UnavailabilitySummary {
                count: reasons.values().sum(),
                reasons,
            },
            statistics,
            raw_points,
        });
    }
    metrics.sort_by(|left, right| {
        left.identity
            .id
            .cmp(&right.identity.id)
            .then_with(|| left.identity.source.cmp(&right.identity.source))
    });

    let snapshot_operation = snapshot_operation(snapshot, cell.operation_id)?;
    let factors = report_factors(cell, plan_operations, snapshot_operation)?;
    let checks = check_summaries(&cell.cell_id, records, &trial_kinds, snapshot_operation);
    let phases = phase_summaries(
        &cell.cell_id,
        run_seed,
        records,
        &trial_kinds,
        &eligible,
        snapshot_operation,
    )?;
    let timelines = resource_timelines(
        &cell.cell_id,
        records,
        &trial_kinds,
        snapshot_operation,
        metric_definitions,
    )?;
    let check_evidence = check_evidence(&cell.cell_id, records, &trial_kinds, snapshot_operation);
    let operation_evidence = operation_evidence(&cell.cell_id, records, &trial_kinds);
    let correlation = cpu_latency_correlation(
        &measured,
        &cpu_by_trial,
        bootstrap_seed(
            run_seed,
            &cell.cell_id,
            "sandbox_cpu_time_vs_batch_makespan",
        ),
    )?;
    let trial_batches =
        u64::from(cell.protocol.warmups).saturating_add(u64::from(cell.protocol.measured_trials));
    let design_counts = ReportDesignCounts {
        test_combinations: 1,
        trial_batches,
        issued_product_requests: trial_batches
            .saturating_mul(u64::from(cell.operation.measured_invocation_count())),
    };
    Ok(CellSummary {
        cell_id: cell.cell_id.clone(),
        family_id: cell.family_id,
        family_label: family_label(snapshot, cell.family_id),
        operation_id: cell.operation_id,
        operation_label: operation_label(snapshot, cell.operation_id),
        comparison_key: cell.comparison_key.clone(),
        design_counts,
        factors,
        counts,
        metrics,
        checks,
        phases,
        timelines,
        check_evidence,
        operation_evidence,
        cpu_latency_correlation: correlation,
    })
}

fn snapshot_operation(
    snapshot: &SnapshotCatalog,
    operation_id: OperationId,
) -> Result<&SnapshotOperation, ReportError> {
    snapshot
        .operations
        .iter()
        .find(|operation| operation.id == operation_id)
        .ok_or_else(|| {
            ReportError::InvalidDefinitionSnapshot(format!(
                "operation {operation_id:?} is absent from the persisted definition snapshot"
            ))
        })
}

fn report_factors(
    cell: &ExpandedCell,
    plan_operations: &[OperationPlan],
    snapshot_operation: &SnapshotOperation,
) -> Result<Vec<ReportFactor>, ReportError> {
    let plan_operation = plan_operations
        .iter()
        .find(|operation| operation.id() == cell.operation_id)
        .ok_or_else(|| {
            ReportError::InvalidManifestAuthority(format!(
                "expanded cell {} has no canonical operation plan",
                cell.cell_id
            ))
        })?;
    let factors = cell
        .operation
        .normalized_factor_projection(plan_operation)
        .map_err(|error| {
            ReportError::InvalidManifestAuthority(format!(
                "cell {} factor projection is invalid: {error}",
                cell.cell_id
            ))
        })?;
    if snapshot_operation.factors.len() != factors.len()
        || snapshot_operation
            .factors
            .iter()
            .map(|factor| factor.id)
            .collect::<BTreeSet<_>>()
            != factors
                .iter()
                .map(|factor| factor.id)
                .collect::<BTreeSet<_>>()
    {
        return Err(ReportError::InvalidManifestAuthority(format!(
            "cell {} factor values do not match its canonical plan and definition snapshot",
            cell.cell_id
        )));
    }

    factors
        .into_iter()
        .map(|factor| {
            let definition = snapshot_operation
                .factors
                .iter()
                .find(|definition| definition.id == factor.id)
                .ok_or_else(|| {
                    ReportError::InvalidDefinitionSnapshot(format!(
                        "operation {:?} is missing factor {:?}",
                        cell.operation_id, factor.id
                    ))
                })?;
            Ok(ReportFactor {
                id: factor.id,
                label: definition.label.clone(),
                help: definition.help.clone(),
                role: factor.role,
                unit: definition.unit.map(factor_display_unit),
                value: factor.value,
                control: factor.control,
            })
        })
        .collect()
}

const fn factor_display_unit(unit: SnapshotFactorUnit) -> FactorDisplayUnit {
    match unit {
        SnapshotFactorUnit::Count => FactorDisplayUnit::Count,
        SnapshotFactorUnit::Bytes => FactorDisplayUnit::Bytes,
        SnapshotFactorUnit::Ratio => FactorDisplayUnit::Ratio,
    }
}

fn factor_studies(
    expanded: &ExpandedPlan,
    cells: &[CellSummary],
) -> Result<Vec<FactorStudyProjection>, ReportError> {
    let mut by_operation = BTreeMap::<OperationId, Vec<&CellSummary>>::new();
    for cell in cells {
        by_operation
            .entry(cell.operation_id)
            .or_default()
            .push(cell);
    }
    let mut studies = Vec::new();
    for (operation_id, mut operation_cells) in by_operation {
        operation_cells.sort_by(|left, right| left.cell_id.cmp(&right.cell_id));
        let first = operation_cells.first().ok_or_else(|| {
            ReportError::InvalidManifestAuthority(format!(
                "operation {operation_id:?} has no expanded cells"
            ))
        })?;
        let operation_label = first.operation_label.clone();
        let varied_factor_ids = first
            .factors
            .iter()
            .filter(|factor| factor.role == FactorRole::Varied)
            .map(|factor| factor.id)
            .collect::<Vec<_>>();
        let controlled_factor_ids = first
            .factors
            .iter()
            .filter(|factor| factor.role == FactorRole::Controlled)
            .map(|factor| factor.id)
            .collect::<Vec<_>>();
        let layout = match varied_factor_ids.as_slice() {
            [] => FactorStudyLayout::SingleCell,
            [factor_id] => FactorStudyLayout::Trend {
                factor_id: *factor_id,
            },
            [row_factor_id, column_factor_id] => FactorStudyLayout::Matrix {
                row_factor_id: *row_factor_id,
                column_factor_id: *column_factor_id,
            },
            factor_ids => FactorStudyLayout::SmallMultiples {
                factor_ids: factor_ids.to_vec(),
            },
        };
        for cell in &operation_cells {
            let roles = cell
                .factors
                .iter()
                .map(|factor| (factor.id, factor.role))
                .collect::<Vec<_>>();
            let expected_roles = first
                .factors
                .iter()
                .map(|factor| (factor.id, factor.role))
                .collect::<Vec<_>>();
            if roles != expected_roles {
                return Err(ReportError::InvalidManifestAuthority(format!(
                    "operation {operation_id:?} has inconsistent factor roles in cell {}",
                    cell.cell_id
                )));
            }
        }

        // A factor projection is a statistical cohort, so only identities present
        // unchanged in every operation cell are projected. Per-cell summaries still
        // retain metrics whose source/availability semantics differ.
        let metric_identities = first
            .metrics
            .iter()
            .map(|metric| metric.identity.clone())
            .filter(|identity| {
                operation_cells.iter().all(|cell| {
                    cell.metrics
                        .iter()
                        .any(|metric| metric.identity == *identity)
                })
            })
            .collect::<Vec<_>>();
        if !metric_identities
            .iter()
            .any(|identity| identity.id == PRIMARY_LATENCY_METRIC_ID)
        {
            return Err(ReportError::InvalidManifestAuthority(format!(
                "operation {operation_id:?} has no compatible primary latency projection"
            )));
        }

        for metric_identity in metric_identities {
            let mut projected_cells = Vec::with_capacity(operation_cells.len());
            for cell in &operation_cells {
                let metric = cell
                    .metrics
                    .iter()
                    .find(|metric| metric.identity == metric_identity)
                    .ok_or_else(|| {
                        ReportError::InvalidManifestAuthority(format!(
                            "cell {} lost compatible metric {} while building factor studies",
                            cell.cell_id, metric_identity.id
                        ))
                    })?;
                projected_cells.push(FactorStudyCell {
                    cell_id: cell.cell_id.clone(),
                    factors: cell.factors.clone(),
                    successful_n: metric.available_n,
                    failed_n: metric.failed_n,
                    median: metric.statistics.median,
                    confidence_interval: metric.statistics.median_confidence_interval.map(
                        |interval| ReportConfidenceInterval {
                            level: interval.level,
                            lower: interval.lower,
                            upper: interval.upper,
                            method: ReportConfidenceMethod::PercentileBootstrapMedian,
                            resamples: interval.resamples,
                        },
                    ),
                    interval_omission_reason: metric.statistics.confidence_interval_omission.map(
                        |reason| match reason {
                            ConfidenceIntervalOmission::InsufficientN => {
                                "insufficient_n".to_owned()
                            }
                        },
                    ),
                    raw_points: metric.raw_points.clone(),
                });
            }
            let control_comparisons = factor_control_comparisons(
                expanded.canonical_plan.seed,
                operation_id,
                &metric_identity,
                &projected_cells,
            )?;
            studies.push(FactorStudyProjection {
                operation_id,
                operation_label: operation_label.clone(),
                metric: metric_identity,
                layout: layout.clone(),
                varied_factor_ids: varied_factor_ids.clone(),
                controlled_factor_ids: controlled_factor_ids.clone(),
                cells: projected_cells,
                control_comparisons,
            });
        }
    }
    let primary_cell_count = studies
        .iter()
        .filter(|study| study.metric.id == PRIMARY_LATENCY_METRIC_ID)
        .map(|study| study.cells.len())
        .sum::<usize>();
    if primary_cell_count != expanded.cells.len() {
        return Err(ReportError::InvalidManifestAuthority(
            "primary factor-study projection does not cover every expanded cell".to_owned(),
        ));
    }
    studies.sort_by(|left, right| {
        left.operation_id
            .cmp(&right.operation_id)
            .then_with(|| left.metric.id.cmp(&right.metric.id))
            .then_with(|| left.metric.source.cmp(&right.metric.source))
    });
    Ok(studies)
}

fn factor_control_comparisons(
    run_seed: u64,
    operation_id: OperationId,
    metric: &MetricIdentity,
    cells: &[FactorStudyCell],
) -> Result<Vec<ControlComparisonProjection>, ReportError> {
    let has_varied_factor = cells.first().is_some_and(|cell| {
        cell.factors
            .iter()
            .any(|factor| factor.role == FactorRole::Varied)
    });
    if !has_varied_factor {
        return Ok(Vec::new());
    }
    let controls = cells
        .iter()
        .filter(|cell| {
            cell.factors.iter().all(|factor| match factor.role {
                FactorRole::Controlled => true,
                FactorRole::Varied => factor.control.as_ref() == Some(&factor.value),
            })
        })
        .collect::<Vec<_>>();
    let [control] = controls.as_slice() else {
        return Err(ReportError::InvalidManifestAuthority(format!(
            "operation {operation_id:?} must expand exactly one all-control cell, found {}",
            controls.len()
        )));
    };
    let control_values = control
        .raw_points
        .iter()
        .map(|point| point.value)
        .collect::<Vec<_>>();
    let mut comparisons = Vec::with_capacity(cells.len().saturating_sub(1));
    for candidate in cells {
        if candidate.cell_id == control.cell_id {
            continue;
        }
        let changed_factor_ids = candidate
            .factors
            .iter()
            .filter(|factor| {
                factor.role == FactorRole::Varied
                    && factor
                        .control
                        .as_ref()
                        .is_some_and(|control| control != &factor.value)
            })
            .map(|factor| factor.id)
            .collect::<Vec<_>>();
        let absolute_difference = control
            .median
            .zip(candidate.median)
            .map(|(control, candidate)| candidate - control);
        let percentage_difference = metric
            .ratio_scale
            .then_some(())
            .and_then(|()| control.median.zip(candidate.median))
            .and_then(|(control, candidate)| {
                (control > 0.0).then(|| (candidate - control) / control * 100.0)
            });
        let candidate_values = candidate
            .raw_points
            .iter()
            .map(|point| point.value)
            .collect::<Vec<_>>();
        let seed = bootstrap_seed(
            run_seed,
            &candidate.cell_id,
            &format!("control_difference:{}", metric.id),
        );
        let interval = statistics::bootstrap_median_difference_interval(
            &control_values,
            &candidate_values,
            seed,
        )?;
        let interval_omission_reason = if interval.is_none() {
            Some(
                if control.median.is_none() || candidate.median.is_none() {
                    "missing_median"
                } else {
                    "insufficient_n"
                }
                .to_owned(),
            )
        } else {
            None
        };
        comparisons.push(ControlComparisonProjection {
            comparison_id: sha256_json(&(
                REPORT_SCHEMA_VERSION,
                operation_id,
                &metric.id,
                &metric.source,
                &control.cell_id,
                &candidate.cell_id,
            ))?,
            control_cell_id: control.cell_id.clone(),
            candidate_cell_id: candidate.cell_id.clone(),
            changed_factor_ids,
            control_median: control.median,
            candidate_median: candidate.median,
            absolute_difference,
            percentage_difference,
            median_difference_confidence_interval: interval.map(|interval| {
                ReportConfidenceInterval {
                    level: interval.level,
                    lower: interval.lower,
                    upper: interval.upper,
                    method: ReportConfidenceMethod::PercentileBootstrapMedianDifference,
                    resamples: interval.resamples,
                }
            }),
            interval_omission_reason,
        });
    }
    Ok(comparisons)
}

fn resource_timelines(
    cell_id: &str,
    records: &[ObservationRecord],
    trial_kinds: &BTreeMap<&str, TrialKind>,
    operation: &SnapshotOperation,
    metric_definitions: &BTreeMap<String, &SnapshotMetricDefinition>,
) -> Result<Vec<ResourceTimelineProjection>, ReportError> {
    let mut trial_ids = trial_kinds
        .iter()
        .filter_map(|(trial_id, kind)| {
            (*kind == TrialKind::Measured).then_some((*trial_id).to_owned())
        })
        .collect::<Vec<_>>();
    trial_ids.sort();
    let mut timelines = Vec::with_capacity(trial_ids.len());
    for trial_id in trial_ids {
        let mut request_spans = records
            .iter()
            .filter_map(|record| match record {
                ObservationRecord::Request(request)
                    if request.cell_id == cell_id && request.trial_id == trial_id =>
                {
                    Some(RequestSpanProjection {
                        request_id: request.request_id.clone(),
                        start_offset_ns: request.start_offset_ns,
                        duration_ns: request.latency_ns,
                        succeeded: request.succeeded,
                        status: request.status.clone(),
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        request_spans.sort_by(|left, right| {
            left.start_offset_ns
                .cmp(&right.start_offset_ns)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        let mut phase_spans = records
            .iter()
            .filter_map(|record| match record {
                ObservationRecord::Phase(phase)
                    if phase.cell_id == cell_id && phase.trial_id == trial_id =>
                {
                    let definition = operation
                        .phases
                        .iter()
                        .find(|candidate| candidate.id == phase.id);
                    Some(PhaseSpanProjection {
                        id: phase.id,
                        label: definition.map_or_else(
                            || format!("{:?}", phase.id),
                            |definition| definition.label.clone(),
                        ),
                        help: definition
                            .map_or_else(String::new, |definition| definition.help.clone()),
                        semantic_revision: phase.semantic_revision,
                        request_id: phase.request_id.clone(),
                        start_offset_ns: phase.start_offset_ns,
                        duration_ns: phase.duration_ns,
                        status: phase.status,
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        phase_spans.sort_by(|left, right| {
            left.start_offset_ns
                .cmp(&right.start_offset_ns)
                .then_with(|| left.id.cmp(&right.id))
                .then_with(|| left.request_id.cmp(&right.request_id))
        });

        let mut grouped =
            BTreeMap::<(ResourceKey, Option<String>), Vec<ResourceTimelinePoint>>::new();
        for record in records {
            let ObservationRecord::Resource(observation) = record else {
                continue;
            };
            if observation.cell_id != cell_id || observation.trial_id != trial_id {
                continue;
            }
            let reading = &observation.reading;
            grouped
                .entry((
                    ResourceKey {
                        metric_id: reading.metric_id.clone(),
                        semantic_revision: reading.metric_semantic_revision,
                        unit: reading.unit,
                        scope: reading.scope,
                        kind: reading.kind,
                        aggregation: reading.aggregation,
                        source: reading.source.clone(),
                    },
                    observation.request_id.clone(),
                ))
                .or_default()
                .push(ResourceTimelinePoint {
                    monotonic_offset_ns: reading.monotonic_offset_ns,
                    sampled: reading.sampled,
                    value: reading.value.clone(),
                });
        }
        let mut series = Vec::with_capacity(grouped.len());
        for ((key, request_id), mut points) in grouped {
            points.sort_by_key(|point| point.monotonic_offset_ns);
            let definition = metric_definitions
                .get(&key.metric_id)
                .ok_or_else(|| ReportError::UnknownPersistedMetric(key.metric_id.clone()))?;
            let (label, help) = metric_copy(&key.metric_id);
            series.push(ResourceSeriesProjection {
                identity: MetricIdentity {
                    id: key.metric_id,
                    label: label.to_owned(),
                    help: help.to_owned(),
                    semantic_revision: key.semantic_revision,
                    unit: key.unit,
                    scope: key.scope,
                    kind: key.kind,
                    availability: definition.availability,
                    aggregation: key.aggregation,
                    direction: definition.direction,
                    source: key.source,
                    ratio_scale: ratio_scale(key.unit),
                    report_derivation_revision: REPORT_DERIVATION_REVISION,
                },
                request_id,
                points,
            });
        }
        series.sort_by(|left, right| {
            left.identity
                .id
                .cmp(&right.identity.id)
                .then_with(|| left.identity.source.cmp(&right.identity.source))
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        let operation_window = request_spans
            .iter()
            .map(|span| {
                (
                    span.start_offset_ns,
                    span.start_offset_ns.saturating_add(span.duration_ns),
                )
            })
            .reduce(|left, right| (left.0.min(right.0), left.1.max(right.1)))
            .map(
                |(start_offset_ns, end_offset_ns)| OperationWindowProjection {
                    start_offset_ns,
                    duration_ns: end_offset_ns.saturating_sub(start_offset_ns),
                },
            );
        let offsets = request_spans
            .iter()
            .flat_map(|span| {
                [
                    span.start_offset_ns,
                    span.start_offset_ns.saturating_add(span.duration_ns),
                ]
            })
            .chain(phase_spans.iter().flat_map(|span| {
                [
                    span.start_offset_ns,
                    span.start_offset_ns.saturating_add(span.duration_ns),
                ]
            }))
            .chain(
                series
                    .iter()
                    .flat_map(|series| series.points.iter().map(|point| point.monotonic_offset_ns)),
            )
            .collect::<Vec<_>>();
        let domain_start_ns = offsets.iter().copied().min().unwrap_or(0);
        let domain_end_ns = offsets.iter().copied().max().unwrap_or(domain_start_ns);
        timelines.push(ResourceTimelineProjection {
            trial_id,
            domain_start_ns,
            domain_end_ns,
            operation_window,
            request_spans,
            phase_spans,
            series,
        });
    }
    Ok(timelines)
}

fn check_evidence(
    cell_id: &str,
    records: &[ObservationRecord],
    trial_kinds: &BTreeMap<&str, TrialKind>,
    operation: &SnapshotOperation,
) -> Vec<CheckEvidenceReport> {
    let mut evidence = records
        .iter()
        .filter_map(|record| match record {
            ObservationRecord::Check(result)
                if result.cell_id == cell_id
                    && trial_kinds.get(result.trial_id.as_str()) == Some(&TrialKind::Measured) =>
            {
                let definition = operation.checks.iter().find(|check| check.id == result.id);
                Some(CheckEvidenceReport {
                    id: result.id,
                    label: definition.map_or_else(
                        || format!("{:?}", result.id),
                        |definition| definition.label.clone(),
                    ),
                    help: definition.map_or_else(String::new, |definition| definition.help.clone()),
                    semantic_revision: result.semantic_revision,
                    trial_id: result.trial_id.clone(),
                    request_id: result.request_id.clone(),
                    verdict: result.verdict,
                    duration_ns: result.duration_ns,
                    evidence: result.evidence.clone(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        left.trial_id
            .cmp(&right.trial_id)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    evidence
}

fn operation_evidence(
    cell_id: &str,
    records: &[ObservationRecord],
    trial_kinds: &BTreeMap<&str, TrialKind>,
) -> Vec<OperationEvidenceReport> {
    let mut evidence = records
        .iter()
        .filter_map(|record| match record {
            ObservationRecord::Operation(observation)
                if observation.cell_id == cell_id
                    && trial_kinds.get(observation.trial_id.as_str())
                        == Some(&TrialKind::Measured) =>
            {
                Some(OperationEvidenceReport {
                    trial_id: observation.trial_id.clone(),
                    request_id: observation.request_id.clone(),
                    evidence: observation.evidence.clone(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        left.trial_id
            .cmp(&right.trial_id)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    evidence
}

fn failure_counts(trials: &[&TrialSample]) -> FailureCounts {
    let warmup = trials
        .iter()
        .filter(|trial| trial.kind == TrialKind::Warmup)
        .count();
    let measured = trials
        .iter()
        .copied()
        .filter(|trial| trial.kind == TrialKind::Measured)
        .collect::<Vec<_>>();
    FailureCounts {
        total_attempted: u64::try_from(trials.len()).unwrap_or(u64::MAX),
        warmup: u64::try_from(warmup).unwrap_or(u64::MAX),
        measured_attempted: u64::try_from(measured.len()).unwrap_or(u64::MAX),
        successful: u64::try_from(
            measured
                .iter()
                .filter(|trial| trial_is_eligible(trial))
                .count(),
        )
        .unwrap_or(u64::MAX),
        product_failed: u64::try_from(
            measured
                .iter()
                .filter(|trial| !trial.product_succeeded && !trial.infrastructure_failed)
                .count(),
        )
        .unwrap_or(u64::MAX),
        correctness_failed: u64::try_from(
            measured
                .iter()
                .filter(|trial| correctness_failed(trial))
                .count(),
        )
        .unwrap_or(u64::MAX),
        infrastructure_failed: u64::try_from(
            measured
                .iter()
                .filter(|trial| trial.infrastructure_failed)
                .count(),
        )
        .unwrap_or(u64::MAX),
        cleanup_invalid: u64::try_from(
            measured
                .iter()
                .filter(|trial| !trial.cleanup_baseline_restored)
                .count(),
        )
        .unwrap_or(u64::MAX),
        missing_primary_latency: u64::try_from(
            measured
                .iter()
                .filter(|trial| {
                    trial_is_eligible(trial) && trial.primary_operation_latency_ns.is_none()
                })
                .count(),
        )
        .unwrap_or(u64::MAX),
    }
}

#[derive(Debug)]
struct MetricPointInput {
    trial_id: String,
    request_id: Option<String>,
    value: f64,
    raw_integer_value: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
fn metric_summary(
    cell: &ExpandedCell,
    run_seed: u64,
    id: &str,
    unit: MetricUnit,
    direction: MetricDirection,
    source: &str,
    attempted_n: u64,
    failed_n: u64,
    reasons: BTreeMap<String, u64>,
    points: Vec<MetricPointInput>,
) -> Result<MetricSummary, StatisticsError> {
    let values = points.iter().map(|point| point.value).collect::<Vec<_>>();
    let statistics = statistics::summarize(&values, bootstrap_seed(run_seed, &cell.cell_id, id))?;
    let outliers = statistics
        .outlier_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let raw_points = points
        .into_iter()
        .enumerate()
        .map(|(index, point)| MetricRawPoint {
            trial_id: point.trial_id,
            request_id: point.request_id,
            value: point.value,
            raw_integer_value: point.raw_integer_value,
            outlier: outliers.contains(&index),
        })
        .collect::<Vec<_>>();
    let (label, help) = metric_copy(id);
    Ok(MetricSummary {
        identity: MetricIdentity {
            id: id.to_owned(),
            label: label.to_owned(),
            help: help.to_owned(),
            semantic_revision: PRIMARY_LATENCY_METRIC_REVISION,
            unit,
            scope: MetricScope::Operation,
            kind: MetricKind::Gauge,
            availability: AvailabilityPolicy::ExplicitUnavailable,
            aggregation: AggregationRule::Mean,
            direction,
            source: source.to_owned(),
            ratio_scale: true,
            report_derivation_revision: REPORT_DERIVATION_REVISION,
        },
        attempted_n,
        failed_n,
        available_n: u64::try_from(raw_points.len()).unwrap_or(u64::MAX),
        unavailable: UnavailabilitySummary {
            count: reasons.values().sum(),
            reasons,
        },
        statistics,
        raw_points,
    })
}

fn timing_summaries(
    cell: &ExpandedCell,
    run_seed: u64,
    records: &[ObservationRecord],
    measured: &[&TrialSample],
    counts: &FailureCounts,
) -> Result<Vec<MetricSummary>, ReportError> {
    let eligible = measured
        .iter()
        .copied()
        .filter(|trial| trial_is_eligible(trial))
        .map(|trial| trial.trial_id.as_str())
        .collect::<BTreeSet<_>>();
    let measured_ids = measured
        .iter()
        .map(|trial| trial.trial_id.as_str())
        .collect::<BTreeSet<_>>();

    let batch_points = measured
        .iter()
        .copied()
        .filter(|trial| trial_is_eligible(trial))
        .filter_map(|trial| {
            trial
                .primary_operation_latency_ns
                .map(|value| MetricPointInput {
                    trial_id: trial.trial_id.clone(),
                    request_id: None,
                    value: value as f64,
                    raw_integer_value: Some(value),
                })
        })
        .collect::<Vec<_>>();
    let mut batch_reasons = BTreeMap::new();
    if counts.missing_primary_latency > 0 {
        batch_reasons.insert(
            "missing_batch_makespan".to_owned(),
            counts.missing_primary_latency,
        );
    }

    let measured_requests = records
        .iter()
        .filter_map(|record| match record {
            ObservationRecord::Request(request)
                if request.cell_id == cell.cell_id
                    && measured_ids.contains(request.trial_id.as_str()) =>
            {
                Some(request)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_requests_per_trial = u64::from(cell.operation.measured_invocation_count());
    let request_attempted = u64::try_from(measured.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(expected_requests_per_trial);
    let mut requests_by_trial = BTreeMap::<&str, Vec<_>>::new();
    for request in &measured_requests {
        requests_by_trial
            .entry(request.trial_id.as_str())
            .or_default()
            .push(*request);
    }
    for trial in measured {
        let observed = u64::try_from(
            requests_by_trial
                .get(trial.trial_id.as_str())
                .map_or(0, Vec::len),
        )
        .unwrap_or(u64::MAX);
        if observed > expected_requests_per_trial {
            return Err(invalid_observation(format!(
                "trial {} has {observed} request observations, exceeding its declared issued-request count {expected_requests_per_trial}",
                trial.trial_id
            )));
        }
    }
    let request_points = measured_requests
        .iter()
        .copied()
        .filter(|request| eligible.contains(request.trial_id.as_str()) && request.succeeded)
        .map(|request| MetricPointInput {
            trial_id: request.trial_id.clone(),
            request_id: Some(request.request_id.clone()),
            value: request.latency_ns as f64,
            raw_integer_value: Some(request.latency_ns),
        })
        .collect::<Vec<_>>();
    let failed_requests = measured
        .iter()
        .copied()
        .map(|trial| {
            if !trial_is_eligible(trial) {
                expected_requests_per_trial
            } else {
                u64::try_from(
                    requests_by_trial
                        .get(trial.trial_id.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|request| !request.succeeded)
                        .count(),
                )
                .unwrap_or(u64::MAX)
            }
        })
        .fold(0_u64, u64::saturating_add);
    let missing_eligible_requests = measured
        .iter()
        .copied()
        .filter(|trial| trial_is_eligible(trial))
        .map(|trial| {
            let observed = u64::try_from(
                requests_by_trial
                    .get(trial.trial_id.as_str())
                    .map_or(0, Vec::len),
            )
            .unwrap_or(u64::MAX);
            expected_requests_per_trial.saturating_sub(observed)
        })
        .fold(0_u64, u64::saturating_add);
    let mut request_reasons = BTreeMap::new();
    if missing_eligible_requests > 0 {
        request_reasons.insert(
            "missing_request_observation".to_owned(),
            missing_eligible_requests,
        );
    }

    let mut throughput_points = Vec::new();
    let mut throughput_reasons = BTreeMap::new();
    for trial in measured
        .iter()
        .copied()
        .filter(|trial| trial_is_eligible(trial))
    {
        let requests = requests_by_trial
            .get(trial.trial_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if u64::try_from(requests.len()).unwrap_or(u64::MAX) != expected_requests_per_trial {
            *throughput_reasons
                .entry("missing_request_observation".to_owned())
                .or_default() += 1;
            continue;
        }
        let Some(batch_makespan_ns) = trial.primary_operation_latency_ns else {
            *throughput_reasons
                .entry("missing_batch_makespan".to_owned())
                .or_default() += 1;
            continue;
        };
        if batch_makespan_ns == 0 {
            *throughput_reasons
                .entry("zero_batch_makespan".to_owned())
                .or_default() += 1;
            continue;
        }
        let successful_requests =
            u64::try_from(requests.iter().filter(|request| request.succeeded).count())
                .unwrap_or(u64::MAX);
        throughput_points.push(MetricPointInput {
            trial_id: trial.trial_id.clone(),
            request_id: None,
            value: successful_requests as f64 * 1_000_000_000.0 / batch_makespan_ns as f64,
            raw_integer_value: None,
        });
    }

    let lifecycle_points = |select: fn(&TrialSample) -> u64| {
        measured
            .iter()
            .copied()
            .filter(|trial| trial_is_eligible(trial))
            .map(|trial| {
                let value = select(trial);
                MetricPointInput {
                    trial_id: trial.trial_id.clone(),
                    request_id: None,
                    value: value as f64,
                    raw_integer_value: Some(value),
                }
            })
            .collect::<Vec<_>>()
    };
    let trial_failed = counts.measured_attempted.saturating_sub(counts.successful);
    Ok(vec![
        metric_summary(
            cell,
            run_seed,
            PRIMARY_LATENCY_METRIC_ID,
            MetricUnit::Nanoseconds,
            MetricDirection::LowerIsPreferred,
            "runner_monotonic_batch_barrier",
            counts.measured_attempted,
            trial_failed,
            batch_reasons,
            batch_points,
        )?,
        metric_summary(
            cell,
            run_seed,
            REQUEST_LATENCY_METRIC_ID,
            MetricUnit::Nanoseconds,
            MetricDirection::LowerIsPreferred,
            "runner_monotonic_product_request",
            request_attempted,
            failed_requests,
            request_reasons,
            request_points,
        )?,
        metric_summary(
            cell,
            run_seed,
            THROUGHPUT_METRIC_ID,
            MetricUnit::OperationsPerSecond,
            MetricDirection::HigherIsPreferred,
            "successful_requests_per_batch_makespan",
            counts.measured_attempted,
            trial_failed,
            throughput_reasons,
            throughput_points,
        )?,
        metric_summary(
            cell,
            run_seed,
            SETUP_METRIC_ID,
            MetricUnit::Nanoseconds,
            MetricDirection::DescriptiveOnly,
            "runner_monotonic_lifecycle",
            counts.measured_attempted,
            trial_failed,
            BTreeMap::new(),
            lifecycle_points(|trial| trial.lifecycle.setup_ns),
        )?,
        metric_summary(
            cell,
            run_seed,
            VERIFY_METRIC_ID,
            MetricUnit::Nanoseconds,
            MetricDirection::DescriptiveOnly,
            "runner_monotonic_lifecycle",
            counts.measured_attempted,
            trial_failed,
            BTreeMap::new(),
            lifecycle_points(|trial| trial.lifecycle.verify_ns),
        )?,
        metric_summary(
            cell,
            run_seed,
            TEARDOWN_METRIC_ID,
            MetricUnit::Nanoseconds,
            MetricDirection::DescriptiveOnly,
            "runner_monotonic_lifecycle",
            counts.measured_attempted,
            trial_failed,
            BTreeMap::new(),
            lifecycle_points(|trial| trial.lifecycle.teardown_ns),
        )?,
    ])
}

fn check_summaries(
    cell_id: &str,
    records: &[ObservationRecord],
    trial_kinds: &BTreeMap<&str, TrialKind>,
    operation: &SnapshotOperation,
) -> Vec<CheckSummary> {
    let mut summaries = BTreeMap::<(CheckId, u32), CheckSummary>::new();
    for record in records {
        let ObservationRecord::Check(result) = record else {
            continue;
        };
        if result.cell_id != cell_id
            || trial_kinds.get(result.trial_id.as_str()) != Some(&TrialKind::Measured)
        {
            continue;
        }
        let summary = summaries
            .entry((result.id, result.semantic_revision))
            .or_insert_with(|| {
                let definition = operation.checks.iter().find(|check| check.id == result.id);
                CheckSummary {
                    id: result.id,
                    label: definition
                        .map_or_else(|| format!("{:?}", result.id), |check| check.label.clone()),
                    help: definition.map_or_else(String::new, |check| check.help.clone()),
                    semantic_revision: result.semantic_revision,
                    attempted: 0,
                    passed: 0,
                    failed: 0,
                }
            });
        summary.attempted = summary.attempted.saturating_add(1);
        match result.verdict {
            CheckVerdict::Pass => summary.passed = summary.passed.saturating_add(1),
            CheckVerdict::Fail => summary.failed = summary.failed.saturating_add(1),
        }
    }
    summaries.into_values().collect()
}

fn phase_summaries(
    cell_id: &str,
    run_seed: u64,
    records: &[ObservationRecord],
    trial_kinds: &BTreeMap<&str, TrialKind>,
    eligible: &BTreeSet<&str>,
    operation: &SnapshotOperation,
) -> Result<Vec<PhaseSummary>, StatisticsError> {
    let mut grouped = BTreeMap::<
        (
            PhaseId,
            u32,
            PhaseUnit,
            PhaseSource,
            PhaseCorrelationRule,
            String,
        ),
        (u64, u64, Vec<f64>),
    >::new();
    for record in records {
        let ObservationRecord::Phase(phase) = record else {
            continue;
        };
        if phase.cell_id != cell_id
            || trial_kinds.get(phase.trial_id.as_str()) != Some(&TrialKind::Measured)
        {
            continue;
        }
        let summary = grouped
            .entry((
                phase.id,
                phase.semantic_revision,
                phase.unit,
                phase.source,
                phase.correlation,
                phase.trace_span_name.clone(),
            ))
            .or_insert((0, 0, Vec::new()));
        summary.0 = summary.0.saturating_add(1);
        if phase.status != PhaseStatus::Succeeded {
            summary.1 = summary.1.saturating_add(1);
        } else if eligible.contains(phase.trial_id.as_str()) {
            summary.2.push(phase.duration_ns as f64);
        }
    }
    grouped
        .into_iter()
        .map(
            |(
                (id, revision, unit, source, correlation, trace_span_name),
                (attempted, failed, values),
            )| {
                let definition = operation.phases.iter().find(|phase| phase.id == id);
                Ok(PhaseSummary {
                    id,
                    label: definition
                        .map_or_else(|| format!("{id:?}"), |phase| phase.label.clone()),
                    help: definition.map_or_else(String::new, |phase| phase.help.clone()),
                    semantic_revision: revision,
                    unit,
                    source,
                    correlation,
                    trace_span_name,
                    attempted,
                    failed,
                    duration: statistics::summarize(
                        &values,
                        bootstrap_seed(run_seed, cell_id, &format!("phase:{id:?}:{revision}")),
                    )?,
                })
            },
        )
        .collect()
}

fn aggregate_resource(
    readings: &[&ResourceReading],
    aggregation: AggregationRule,
) -> TrialAggregate {
    let mut sorted = readings.to_vec();
    sorted.sort_by_key(|reading| reading.monotonic_offset_ns);
    let mut values = Vec::with_capacity(sorted.len());
    for reading in &sorted {
        match &reading.value {
            Availability::Available { value } if value.is_finite() => values.push(*value),
            Availability::Available { .. } => {
                return TrialAggregate::Unavailable("non_finite_reading".to_owned());
            }
            Availability::Unavailable { source, reason } => {
                return TrialAggregate::Unavailable(format!("{source}:{reason}"));
            }
        }
    }
    if values.is_empty() {
        return TrialAggregate::Unavailable("missing_reading".to_owned());
    }
    match aggregation {
        AggregationRule::Maximum => TrialAggregate::Available(
            values
                .into_iter()
                .max_by(f64::total_cmp)
                .unwrap_or_default(),
        ),
        AggregationRule::Minimum => TrialAggregate::Available(
            values
                .into_iter()
                .min_by(f64::total_cmp)
                .unwrap_or_default(),
        ),
        AggregationRule::Mean => {
            let count = values.len() as f64;
            TrialAggregate::Available(values.into_iter().sum::<f64>() / count)
        }
        AggregationRule::Delta => {
            if values.len() < 2 {
                return TrialAggregate::Unavailable("insufficient_counter_samples".to_owned());
            }
            let delta = values[values.len() - 1] - values[0];
            if delta < 0.0 {
                TrialAggregate::Unavailable("counter_reset_or_regression".to_owned())
            } else {
                TrialAggregate::Available(delta)
            }
        }
        AggregationRule::Integral => {
            if sorted.len() < 2 {
                return TrialAggregate::Unavailable("insufficient_integral_samples".to_owned());
            }
            let area = sorted
                .windows(2)
                .zip(values.windows(2))
                .map(|(readings, values)| {
                    let elapsed_seconds = readings[1]
                        .monotonic_offset_ns
                        .saturating_sub(readings[0].monotonic_offset_ns)
                        as f64
                        / 1_000_000_000.0;
                    (values[0] + values[1]) * 0.5 * elapsed_seconds
                })
                .sum();
            TrialAggregate::Available(area)
        }
    }
}

fn cpu_latency_correlation(
    measured: &[&TrialSample],
    cpu_by_trial: &BTreeMap<String, TrialAggregate>,
    bootstrap_seed: u64,
) -> Result<CpuLatencyCorrelation, StatisticsError> {
    let mut points = Vec::new();
    let mut exclusions = CorrelationExclusions {
        ineligible_trial: 0,
        missing_latency: 0,
        missing_cpu: 0,
        unavailable_cpu: 0,
    };
    for trial in measured {
        if !trial_is_eligible(trial) {
            exclusions.ineligible_trial = exclusions.ineligible_trial.saturating_add(1);
            continue;
        }
        let Some(latency) = trial.primary_operation_latency_ns else {
            exclusions.missing_latency = exclusions.missing_latency.saturating_add(1);
            continue;
        };
        match cpu_by_trial.get(&trial.trial_id) {
            Some(TrialAggregate::Available(cpu)) => points.push(CorrelationPoint {
                trial_id: trial.trial_id.clone(),
                operation_latency_ns: latency as f64,
                sandbox_cpu_time_ns: *cpu,
            }),
            Some(TrialAggregate::Unavailable(_)) => {
                exclusions.unavailable_cpu = exclusions.unavailable_cpu.saturating_add(1);
            }
            None => exclusions.missing_cpu = exclusions.missing_cpu.saturating_add(1),
        }
    }
    let estimate = statistics::bootstrap_pearson_interval(
        &points
            .iter()
            .map(|point| (point.operation_latency_ns, point.sandbox_cpu_time_ns))
            .collect::<Vec<_>>(),
        bootstrap_seed,
    )?;
    Ok(CpuLatencyCorrelation {
        semantic_revision: 1,
        method: CorrelationMethod::Pearson,
        alignment: CorrelationAlignment::EligibleTrialAggregateByTrialId,
        eligibility: CorrelationEligibility::MeasuredProductSuccessChecksPassCleanupRestored,
        latency_metric_id: PRIMARY_LATENCY_METRIC_ID.to_owned(),
        cpu_metric_id: "sandbox_cpu_time_ns".to_owned(),
        support_count: u64::try_from(points.len()).unwrap_or(u64::MAX),
        coefficient: pearson(&points),
        confidence_interval: estimate.interval,
        interval_omission: estimate.omission,
        points,
        exclusions,
    })
}

fn pearson(points: &[CorrelationPoint]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let count = points.len() as f64;
    let mean_x = points
        .iter()
        .map(|point| point.operation_latency_ns)
        .sum::<f64>()
        / count;
    let mean_y = points
        .iter()
        .map(|point| point.sandbox_cpu_time_ns)
        .sum::<f64>()
        / count;
    let numerator = points
        .iter()
        .map(|point| (point.operation_latency_ns - mean_x) * (point.sandbox_cpu_time_ns - mean_y))
        .sum::<f64>();
    let denominator_x = points
        .iter()
        .map(|point| (point.operation_latency_ns - mean_x).powi(2))
        .sum::<f64>();
    let denominator_y = points
        .iter()
        .map(|point| (point.sandbox_cpu_time_ns - mean_y).powi(2))
        .sum::<f64>();
    let denominator = (denominator_x * denominator_y).sqrt();
    (denominator > 0.0).then_some(numerator / denominator)
}

fn report_rows(cells: &[CellSummary]) -> Result<Vec<ReportResultRow>, serde_json::Error> {
    let mut rows = Vec::new();
    for cell in cells {
        for metric in &cell.metrics {
            rows.push(ReportResultRow {
                row_id: sha256_json(&(
                    &cell.cell_id,
                    &metric.identity.id,
                    metric.identity.semantic_revision,
                    &metric.identity.source,
                ))?,
                operation_id: cell.operation_id,
                cell_id: cell.cell_id.clone(),
                metric_id: metric.identity.id.clone(),
                unit: metric.identity.unit,
                successful_n: metric.available_n,
                failed_n: metric.failed_n,
                unavailable_n: metric.unavailable.count,
                median: metric.statistics.median,
                confidence_interval: metric
                    .statistics
                    .median_confidence_interval
                    .map(|interval| ReportConfidenceInterval {
                        level: interval.level,
                        lower: interval.lower,
                        upper: interval.upper,
                        method: ReportConfidenceMethod::PercentileBootstrapMedian,
                        resamples: interval.resamples,
                    }),
                interval_omission_reason: metric.statistics.confidence_interval_omission.map(
                    |reason| match reason {
                        ConfidenceIntervalOmission::InsufficientN => "insufficient_n".to_owned(),
                    },
                ),
                direction: metric.identity.direction,
            });
        }
    }
    Ok(rows)
}

fn validate_artifact_references(
    store: &ArtifactStore,
    run_id: &str,
    records: &[ObservationRecord],
) -> Result<(), ReportError> {
    let operation_evidence = records
        .iter()
        .filter_map(|record| match record {
            ObservationRecord::Operation(observation) => Some((
                (observation.cell_id.as_str(), observation.trial_id.as_str()),
                &observation.evidence,
            )),
            ObservationRecord::Trial(_)
            | ObservationRecord::Request(_)
            | ObservationRecord::Resource(_)
            | ObservationRecord::Phase(_)
            | ObservationRecord::Check(_) => None,
        })
        .collect::<BTreeMap<_, _>>();

    for trial in records.iter().filter_map(|record| match record {
        ObservationRecord::Trial(trial) => Some(trial),
        ObservationRecord::Request(_)
        | ObservationRecord::Resource(_)
        | ObservationRecord::Phase(_)
        | ObservationRecord::Check(_)
        | ObservationRecord::Operation(_) => None,
    }) {
        let mut seen = BTreeSet::new();
        for reference in &trial.artifacts {
            let invalid = |reason: String| ReportError::InvalidArtifactReference {
                trial_id: trial.trial_id.clone(),
                artifact_id: reference.artifact_id.clone(),
                reason,
            };
            if !seen.insert(reference.artifact_id.as_str()) {
                return Err(invalid("duplicate reference".to_owned()));
            }
            let content = store.content(run_id, &reference.artifact_id)?;
            if content.id != ArtifactId::BoundedEvidence {
                return Err(invalid(
                    "reference does not identify bounded evidence".to_owned(),
                ));
            }
            if content.media_type != reference.media_type {
                return Err(invalid(format!(
                    "media type mismatch: expected {}, received {}",
                    reference.media_type, content.media_type
                )));
            }
            let size = u64::try_from(content.bytes.len()).unwrap_or(u64::MAX);
            if size != reference.size_bytes {
                return Err(invalid(format!(
                    "size mismatch: expected {}, received {size}",
                    reference.size_bytes
                )));
            }
            let digest = sha256_bytes(&content.bytes);
            if digest != reference.sha256 {
                return Err(invalid(format!(
                    "sha256 mismatch: expected {}, received {digest}",
                    reference.sha256
                )));
            }
            let envelope: SchemaEnvelope<OperationEvidence> =
                serde_json::from_slice(&content.bytes).map_err(|error| {
                    invalid(format!("operation evidence envelope is invalid: {error}"))
                })?;
            if envelope.schema_name != BOUNDED_EVIDENCE_SCHEMA_NAME
                || envelope.schema_version != BOUNDED_EVIDENCE_SCHEMA_VERSION
            {
                return Err(invalid(format!(
                    "schema mismatch: received {} v{}",
                    envelope.schema_name, envelope.schema_version
                )));
            }
            if envelope.data.id() != trial.operation_id {
                return Err(invalid(
                    "typed evidence operation does not match trial".to_owned(),
                ));
            }
            let inline = operation_evidence
                .get(&(trial.cell_id.as_str(), trial.trial_id.as_str()))
                .ok_or_else(|| {
                    invalid("referenced evidence lacks its typed observation".to_owned())
                })?;
            if **inline != envelope.data {
                return Err(invalid(
                    "referenced evidence differs from its typed observation".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_observation_links(
    expanded: &ExpandedPlan,
    records: &[ObservationRecord],
    snapshot: &SnapshotCatalog,
) -> Result<(), ReportError> {
    let cells = expanded
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell.operation_id))
        .collect::<BTreeMap<_, _>>();
    let mut trials = BTreeMap::<(&str, &str), OperationId>::new();
    for record in records {
        let ObservationRecord::Trial(trial) = record else {
            continue;
        };
        let expected_operation = cells.get(trial.cell_id.as_str()).ok_or_else(|| {
            invalid_observation(format!(
                "trial {} references unknown cell {}",
                trial.trial_id, trial.cell_id
            ))
        })?;
        if trial.operation_id != *expected_operation {
            return Err(invalid_observation(format!(
                "trial {} operation does not match cell {}",
                trial.trial_id, trial.cell_id
            )));
        }
        if trials
            .insert(
                (trial.cell_id.as_str(), trial.trial_id.as_str()),
                trial.operation_id,
            )
            .is_some()
        {
            return Err(invalid_observation(format!(
                "trial {} is duplicated in cell {}",
                trial.trial_id, trial.cell_id
            )));
        }
    }

    let mut requests = BTreeSet::<(&str, &str, &str)>::new();
    for record in records {
        let ObservationRecord::Request(request) = record else {
            continue;
        };
        let operation =
            linked_trial_operation(&trials, &request.cell_id, &request.trial_id, "request")?;
        if request.operation_id != operation || blank(&request.request_id) {
            return Err(invalid_observation(format!(
                "request {} has invalid operation or request identity",
                request.request_id
            )));
        }
        if !requests.insert((
            request.cell_id.as_str(),
            request.trial_id.as_str(),
            request.request_id.as_str(),
        )) {
            return Err(invalid_observation(format!(
                "request {} is duplicated in trial {}",
                request.request_id, request.trial_id
            )));
        }
    }

    let mut operation_records = BTreeSet::<(&str, &str, Option<&str>)>::new();
    for record in records {
        match record {
            ObservationRecord::Operation(observation) => {
                validate_operation_observation(observation, &trials, &requests)?;
                if !operation_records.insert((
                    observation.cell_id.as_str(),
                    observation.trial_id.as_str(),
                    observation.request_id.as_deref(),
                )) {
                    return Err(invalid_observation(format!(
                        "operation evidence is duplicated in trial {}",
                        observation.trial_id
                    )));
                }
            }
            ObservationRecord::Phase(phase) => {
                let operation =
                    linked_trial_operation(&trials, &phase.cell_id, &phase.trial_id, "phase")?;
                let definition = snapshot
                    .operations
                    .iter()
                    .find(|definition| definition.id == operation)
                    .and_then(|definition| {
                        definition
                            .phases
                            .iter()
                            .find(|definition| definition.id == phase.id)
                    })
                    .ok_or_else(|| {
                        invalid_observation(format!(
                            "persisted phase {:?} is absent from its operation definition snapshot",
                            phase.id
                        ))
                    })?;
                if phase.semantic_revision != definition.semantic_revision
                    || phase.unit != definition.unit
                    || phase.source != definition.source
                    || phase.correlation != definition.correlation
                    || phase.trace_span_name != definition.trace_span_name
                {
                    return Err(invalid_observation(format!(
                        "persisted phase {:?} does not match its operation definition snapshot",
                        phase.id
                    )));
                }
                match phase.correlation {
                    PhaseCorrelationRule::ExactRequestTraceSpan => {
                        let request_id = phase.request_id.as_deref().filter(|id| !blank(id));
                        if request_id.is_none_or(|request_id| {
                            !requests.contains(&(
                                phase.cell_id.as_str(),
                                phase.trial_id.as_str(),
                                request_id,
                            ))
                        }) {
                            return Err(invalid_observation(format!(
                                "phase {:?} lacks its exact correlated request",
                                phase.id
                            )));
                        }
                    }
                }
            }
            ObservationRecord::Trial(_)
            | ObservationRecord::Request(_)
            | ObservationRecord::Resource(_)
            | ObservationRecord::Check(_) => {}
        }
    }
    Ok(())
}

fn validate_operation_observation<'a>(
    observation: &'a OperationObservation,
    trials: &BTreeMap<(&'a str, &'a str), OperationId>,
    requests: &BTreeSet<(&'a str, &'a str, &'a str)>,
) -> Result<(), ReportError> {
    let operation = linked_trial_operation(
        trials,
        &observation.cell_id,
        &observation.trial_id,
        "operation evidence",
    )?;
    if observation.operation_id != operation || observation.evidence.id() != operation {
        return Err(invalid_observation(format!(
            "operation evidence in trial {} does not match its cell operation",
            observation.trial_id
        )));
    }
    if let Some(request_id) = observation.request_id.as_deref() {
        if blank(request_id)
            || !requests.contains(&(
                observation.cell_id.as_str(),
                observation.trial_id.as_str(),
                request_id,
            ))
        {
            return Err(invalid_observation(format!(
                "operation evidence in trial {} references an unknown request",
                observation.trial_id
            )));
        }
    }
    Ok(())
}

fn linked_trial_operation(
    trials: &BTreeMap<(&str, &str), OperationId>,
    cell_id: &str,
    trial_id: &str,
    record_kind: &str,
) -> Result<OperationId, ReportError> {
    trials.get(&(cell_id, trial_id)).copied().ok_or_else(|| {
        invalid_observation(format!(
            "{record_kind} references unknown trial {trial_id} in cell {cell_id}"
        ))
    })
}

fn validate_manifest_authority(
    manifest: &RunManifest,
    expanded: &ExpandedPlan,
    snapshot: &SnapshotCatalog,
    definition_snapshot_sha256: &str,
) -> Result<(), ReportError> {
    let invalid = |detail: String| ReportError::InvalidManifestAuthority(detail);
    if manifest.definition_snapshot.schema_name != DEFINITION_SNAPSHOT_SCHEMA_NAME
        || manifest.definition_snapshot.schema_version != snapshot.schema_version
        || manifest.definition_snapshot.sha256 != definition_snapshot_sha256
    {
        return Err(invalid(
            "definition snapshot identity or content hash does not match the persisted artifact"
                .to_owned(),
        ));
    }
    if blank(&manifest.producer.package)
        || blank(&manifest.producer.version)
        || manifest.treatment != manifest.environment.treatment
        || manifest.fixture_generator_revision == 0
    {
        return Err(invalid(
            "producer, treatment, or fixture-generator identity is incomplete".to_owned(),
        ));
    }
    if manifest.environment.client_cohort != expanded.effective_environment.client_cohort
        || manifest.environment.workspace_root_identity
            != expanded.effective_environment.workspace_root_identity
        || manifest.environment.image_digest != expanded.effective_environment.image_digest
        || manifest.environment.host.filesystem != expanded.effective_environment.filesystem
        || manifest.environment.host.free_space_bytes
            != expanded.effective_environment.free_space_bytes
    {
        return Err(invalid(
            "redacted environment does not match the expanded effective environment".to_owned(),
        ));
    }

    validate_artifact_schema(
        &manifest.artifact_schemas.run_manifest,
        RUN_MANIFEST_SCHEMA_NAME,
        RUN_MANIFEST_SCHEMA_VERSION,
        &[RUN_MANIFEST_SCHEMA_VERSION],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.intent_plan,
        INTENT_PLAN_SCHEMA_NAME,
        expanded.canonical_plan.schema_version,
        &[expanded.canonical_plan.schema_version],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.expanded_plan,
        EXPANDED_PLAN_SCHEMA_NAME,
        expanded.schema_version,
        &[expanded.schema_version],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.definition_snapshot,
        DEFINITION_SNAPSHOT_SCHEMA_NAME,
        snapshot.schema_version,
        &[snapshot.schema_version],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.environment_metadata,
        ENVIRONMENT_METADATA_SCHEMA_NAME,
        manifest.environment.schema_version,
        &[manifest.environment.schema_version],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.events,
        crate::events::EVENT_SCHEMA_NAME,
        crate::events::EVENT_SCHEMA_VERSION,
        &[crate::events::EVENT_SCHEMA_VERSION],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.observations,
        OBSERVATION_SCHEMA_NAME,
        OBSERVATION_SCHEMA_VERSION,
        &[1, 2, 3],
    )?;
    validate_artifact_schema(
        &manifest.artifact_schemas.bounded_evidence,
        BOUNDED_EVIDENCE_SCHEMA_NAME,
        BOUNDED_EVIDENCE_SCHEMA_VERSION,
        &[BOUNDED_EVIDENCE_SCHEMA_VERSION],
    )?;

    if manifest.fixed_lifecycle_policy != expanded.fixed_lifecycle_policy
        || manifest.failure_policy.semantic_revision
            != manifest.fixed_lifecycle_policy.failure_revision
        || manifest.failure_policy.automatic_measured_operation_retries
            != manifest.fixed_lifecycle_policy.automatic_retries
        || manifest
            .failure_policy
            .product_transport_timeout_or_correctness
            != crate::scheduler::FailureAction::ContinueWhenEnvironmentSafe
        || manifest
            .failure_policy
            .fixture_containment_ownership_environment_or_infrastructure
            != crate::scheduler::FailureAction::AbortCampaign
        || manifest.failure_policy.teardown_or_cleanup_baseline
            != crate::scheduler::FailureAction::AbortCampaign
    {
        return Err(invalid(
            "lifecycle and failure policy do not match the expanded plan".to_owned(),
        ));
    }
    let timeout_values = [
        manifest.effective_timeouts.sandbox_create_timeout_ms,
        manifest.effective_timeouts.sandbox_destroy_timeout_ms,
        manifest.effective_timeouts.operation_teardown_timeout_ms,
        manifest.effective_timeouts.gateway_stop_timeout_ms,
        manifest
            .effective_timeouts
            .gateway_owned_resource_cleanup_timeout_ms,
        manifest.effective_timeouts.gateway_shutdown_timeout_ms,
        manifest.effective_timeouts.gateway_log_drain_timeout_ms,
        manifest
            .effective_timeouts
            .layerstack_observation_timeout_ms,
        manifest
            .effective_timeouts
            .layerstack_trace_retry_timeout_ms,
        manifest
            .effective_timeouts
            .product_resource_observation_timeout_ms,
    ];
    if timeout_values.contains(&0) {
        return Err(invalid(
            "effective timeout policy contains a zero duration".to_owned(),
        ));
    }

    let selected = expanded
        .cells
        .iter()
        .map(|cell| cell.operation_id)
        .collect::<BTreeSet<_>>();
    let expected_operation_order = OperationId::ALL
        .into_iter()
        .filter(|operation| selected.contains(operation))
        .collect::<Vec<_>>();
    let actual_operation_order = manifest
        .operation_authorities
        .iter()
        .map(|authority| authority.operation_id)
        .collect::<Vec<_>>();
    if actual_operation_order != expected_operation_order {
        return Err(invalid(
            "operation authority entries must cover selected operations in closed order".to_owned(),
        ));
    }
    for authority in &manifest.operation_authorities {
        let definition = snapshot
            .operations
            .iter()
            .find(|definition| definition.id == authority.operation_id)
            .ok_or_else(|| {
                invalid(format!(
                    "missing definition for {:?}",
                    authority.operation_id
                ))
            })?;
        let cells = expanded
            .cells
            .iter()
            .filter(|cell| cell.operation_id == authority.operation_id)
            .collect::<Vec<_>>();
        let mut isolation = Vec::<ResolvedIsolationPolicy>::new();
        let mut timeouts = BTreeSet::new();
        for cell in cells {
            let resolved = cell.operation.resolved_isolation();
            if !isolation.contains(&resolved) {
                isolation.push(resolved);
            }
            timeouts.insert(cell.protocol.timeout_ms);
            if cell.family_id != definition.family
                || cell.operation_semantic_revision != definition.semantic_revision
                || cell.factor_schema_revision != definition.factor_schema_revision
                || cell.comparison_key.comparison_projection_revision
                    != definition.comparison.semantic_revision
                || cell.comparison_key.product_access != definition.product_access
                || cell.comparison_key.count_semantics != definition.count_semantics
                || cell.protocol.cleanup != definition.cleanup
            {
                return Err(invalid(format!(
                    "expanded cell {} disagrees with its persisted definition",
                    cell.cell_id
                )));
            }
        }
        if authority.family_id != definition.family
            || authority.semantic_revision != definition.semantic_revision
            || authority.factor_schema_revision != definition.factor_schema_revision
            || authority.comparison_projection_revision != definition.comparison.semantic_revision
            || authority.client_cohort != manifest.environment.client_cohort
            || !definition
                .supported_cohorts
                .contains(&authority.client_cohort)
            || authority.product_access != definition.product_access
            || authority.count_semantics != definition.count_semantics
            || authority.cleanup_policy != definition.cleanup
            || authority.resolved_isolation_policies != isolation
            || authority.request_timeout_ms != timeouts.into_iter().collect::<Vec<_>>()
            || !valid_stabilization_policy(
                authority.operation_id,
                authority.stabilization_policy,
                manifest.fixed_lifecycle_policy.stabilization_revision,
            )
        {
            return Err(invalid(format!(
                "operation authority for {:?} is incomplete or inconsistent",
                authority.operation_id
            )));
        }
    }

    let expected_metrics = snapshot
        .metrics
        .iter()
        .map(|metric| (metric.id.clone(), metric.semantic_revision))
        .collect::<BTreeMap<_, _>>();
    let actual_metrics = manifest
        .metric_revisions
        .iter()
        .map(|metric| (metric.metric_id.clone(), metric.semantic_revision))
        .collect::<BTreeMap<_, _>>();
    if expected_metrics.len() != manifest.metric_revisions.len()
        || actual_metrics != expected_metrics
    {
        return Err(invalid(
            "metric revision identities do not match the definition snapshot".to_owned(),
        ));
    }
    let mut expected_checks = BTreeMap::new();
    let mut expected_phases = BTreeMap::new();
    for operation in snapshot
        .operations
        .iter()
        .filter(|operation| selected.contains(&operation.id))
    {
        for check in &operation.checks {
            expected_checks.insert(check.id, check.semantic_revision);
        }
        for phase in &operation.phases {
            expected_phases.insert(phase.id, phase.semantic_revision);
        }
    }
    let actual_checks = manifest
        .check_revisions
        .iter()
        .map(|check| (check.check_id, check.semantic_revision))
        .collect::<BTreeMap<_, _>>();
    let actual_phases = manifest
        .phase_revisions
        .iter()
        .map(|phase| (phase.phase_id, phase.semantic_revision))
        .collect::<BTreeMap<_, _>>();
    if actual_checks.len() != manifest.check_revisions.len()
        || actual_checks != expected_checks
        || actual_phases.len() != manifest.phase_revisions.len()
        || actual_phases != expected_phases
    {
        return Err(invalid(
            "check or phase revision identities do not match selected definitions".to_owned(),
        ));
    }

    validate_gateway_authority(manifest, expanded)?;
    validate_cap_authority(manifest, expanded)?;
    for (fixture_id, fixture_hash) in &manifest.fixture_hashes {
        if blank(fixture_id) || !valid_sha256(fixture_hash) {
            return Err(invalid(
                "fixture identities must be non-empty sha256 values".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_artifact_schema(
    identity: &ArtifactSchemaIdentity,
    schema_name: &str,
    write_version: u32,
    read_versions: &[u32],
) -> Result<(), ReportError> {
    if identity.schema_name != schema_name
        || identity.write_version != write_version
        || identity.read_versions != read_versions
    {
        return Err(ReportError::InvalidManifestAuthority(format!(
            "artifact schema declaration for {schema_name} is inconsistent"
        )));
    }
    Ok(())
}

fn valid_stabilization_policy(
    operation: OperationId,
    policy: StabilizationPolicy,
    expected_revision: u32,
) -> bool {
    match (operation, policy) {
        (
            OperationId::ExecCommand
            | OperationId::FileRead
            | OperationId::FileWrite
            | OperationId::FileEdit
            | OperationId::FileBlame
            | OperationId::CreateWorkspace,
            StabilizationPolicy::NotRequired { semantic_revision },
        ) => semantic_revision == expected_revision,
        (
            OperationId::SquashLayerstack,
            StabilizationPolicy::ExactSnapshotQuietWindow {
                semantic_revision,
                quiet_window_matches,
                poll_interval_ms,
                timeout_ms,
            },
        ) => {
            semantic_revision == expected_revision
                && quiet_window_matches == 3
                && poll_interval_ms == 100
                && timeout_ms == 5_000
        }
        (
            OperationId::ExecCommand
            | OperationId::FileRead
            | OperationId::FileWrite
            | OperationId::FileEdit
            | OperationId::FileBlame
            | OperationId::CreateWorkspace,
            StabilizationPolicy::ExactSnapshotQuietWindow { .. },
        )
        | (OperationId::SquashLayerstack, StabilizationPolicy::NotRequired { .. }) => false,
    }
}

fn validate_gateway_authority(
    manifest: &RunManifest,
    expanded: &ExpandedPlan,
) -> Result<(), ReportError> {
    let policy = &manifest.gateway_policy;
    let widths_are_canonical = !policy.remount_sweep_widths.is_empty()
        && policy.remount_sweep_widths.iter().all(|width| *width > 0)
        && policy
            .remount_sweep_widths
            .windows(2)
            .all(|window| window[0] < window[1]);
    if policy.semantic_revision == 0
        || policy.mode != expanded.effective_environment.gateway_mode
        || !policy.loopback_only
        || !policy.isolated_runtime_per_execution_block
        || !widths_are_canonical
        || policy.maximum_connections == 0
        || policy.readiness_timeout_ms == 0
        || policy.readiness_probe_timeout_ms == 0
        || policy.readiness_poll_interval_ms == 0
        || policy.readiness_probe_timeout_ms > policy.readiness_timeout_ms
        || policy.readiness_poll_interval_ms > policy.readiness_timeout_ms
    {
        return Err(ReportError::InvalidManifestAuthority(
            "effective loopback gateway policy is incomplete or inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_cap_authority(
    manifest: &RunManifest,
    expanded: &ExpandedPlan,
) -> Result<(), ReportError> {
    let policy = &manifest.safety_policy;
    let campaign = &policy.campaign_caps;
    let campaign_caps_are_valid =
        valid_campaign_cap(
            campaign.expanded_test_combinations,
            expanded.estimates.cell_count,
        ) && valid_campaign_cap(campaign.trial_batches, expanded.estimates.trial_batch_count)
            && valid_campaign_cap(
                campaign.issued_product_requests,
                expanded.estimates.issued_operation_request_count,
            );
    let selected = expanded
        .cells
        .iter()
        .map(|cell| cell.operation_id)
        .collect::<BTreeSet<_>>();
    let mut product_cap_keys = BTreeSet::new();
    let product_caps_are_valid = policy.product_caps.iter().all(|cap| {
        product_cap_keys.insert((cap.operation_id, cap.cap_id))
            && selected.contains(&cap.operation_id)
            && cap.maximum_requested == cap.maximum_effective
            && cap.maximum_effective <= cap.fixed_maximum
            && cap.fixed_maximum > 0
            && cap.cap_revision > 0
            && cap.unit == product_cap_unit(cap.cap_id)
    });
    let fixed = policy.fixed_gateway_caps;
    let fixed_caps_are_valid = [
        fixed.log_bytes,
        fixed.log_line_bytes,
        fixed.product_path_bytes,
        fixed.product_content_bytes,
        fixed.product_edits,
        fixed.command_timeout_ms,
        fixed.product_trace_nodes,
        fixed.product_resource_window_ms,
        fixed.product_resource_samples,
    ]
    .into_iter()
    .all(|value| value > 0);
    if policy.semantic_revision == 0
        || !campaign_caps_are_valid
        || !product_caps_are_valid
        || !fixed_caps_are_valid
    {
        return Err(ReportError::InvalidManifestAuthority(
            "campaign or product cap authority is incomplete or inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn valid_campaign_cap(cap: CapResolution, expected: u64) -> bool {
    cap.requested == expected
        && cap.effective == cap.requested
        && cap.effective <= cap.fixed_maximum
        && cap.unit == CapUnit::Count
        && cap.cap_revision > 0
}

const fn product_cap_unit(cap: ProductCapId) -> CapUnit {
    match cap {
        ProductCapId::StoredCommandOutput
        | ProductCapId::FileReadReturnedBytes
        | ProductCapId::FileWriteContentBytes
        | ProductCapId::FileEditFileBytes
        | ProductCapId::LayerstackPayloadBytes => CapUnit::Bytes,
        ProductCapId::FileEditReplacementCount
        | ProductCapId::FileBlameAuditabilityEvents
        | ProductCapId::LayerstackPreparedLayers => CapUnit::Count,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn validate_snapshot(snapshot: &SnapshotCatalog, expected_version: u32) -> Result<(), ReportError> {
    if snapshot.schema_version != expected_version {
        return Err(ReportError::InvalidDefinitionSnapshot(format!(
            "catalog schema version {} does not match manifest version {expected_version}",
            snapshot.schema_version
        )));
    }
    if snapshot.factor_roles != [FactorRole::Varied, FactorRole::Controlled] {
        return Err(invalid_snapshot(
            "factor roles must contain the closed v1 varied and controlled roles exactly once",
        ));
    }
    snapshot
        .workspace_profiles
        .validate()
        .map_err(|error| invalid_snapshot(format!("workspace profile catalog: {error}")))?;

    let family_ids = snapshot
        .families
        .iter()
        .map(|family| family.id)
        .collect::<BTreeSet<_>>();
    if family_ids.len() != snapshot.families.len() {
        return Err(invalid_snapshot("family ids must be unique"));
    }
    if family_ids != FamilyId::ALL.into_iter().collect() {
        return Err(invalid_snapshot(
            "definition snapshot must contain every closed v1 family exactly once",
        ));
    }
    for family in &snapshot.families {
        if blank(&family.label)
            || blank(&family.help)
            || blank(&family.research_question)
            || blank(&family.measured_boundary)
        {
            return Err(invalid_snapshot(format!(
                "family {:?} scientific metadata is incomplete",
                family.id
            )));
        }
    }

    if snapshot.metrics.is_empty() {
        return Err(invalid_snapshot(
            "definition snapshot must contain registered resource metrics",
        ));
    }
    let mut metric_ids = BTreeSet::new();
    for metric in &snapshot.metrics {
        if blank(&metric.id) || metric.semantic_revision == 0 {
            return Err(invalid_snapshot(
                "resource metric identity and semantic revision are required",
            ));
        }
        if !metric_ids.insert(metric.id.as_str()) {
            return Err(invalid_snapshot(format!(
                "resource metric id {} is duplicated",
                metric.id
            )));
        }
    }

    let operation_ids = snapshot
        .operations
        .iter()
        .map(|operation| operation.id)
        .collect::<BTreeSet<_>>();
    if operation_ids.len() != snapshot.operations.len() {
        return Err(invalid_snapshot("operation ids must be unique"));
    }
    if operation_ids != OperationId::ALL.into_iter().collect() {
        return Err(invalid_snapshot(
            "definition snapshot must contain every closed v1 operation exactly once",
        ));
    }
    for operation in &snapshot.operations {
        validate_snapshot_operation(operation)?;
    }
    Ok(())
}

fn validate_snapshot_operation(operation: &SnapshotOperation) -> Result<(), ReportError> {
    if blank(&operation.label)
        || blank(&operation.help)
        || blank(&operation.measured_boundary)
        || blank(&operation.count_semantics_help)
        || operation.semantic_revision == 0
        || operation.factor_schema_revision == 0
        || operation.comparison.semantic_revision == 0
    {
        return Err(invalid_snapshot(format!(
            "operation {:?} scientific metadata is incomplete",
            operation.id
        )));
    }
    if !operation_identity_matches_v1(operation) {
        return Err(invalid_snapshot(format!(
            "operation {:?} has inconsistent closed v1 family, access, lifecycle, or security metadata",
            operation.id
        )));
    }
    if operation.supported_cohorts.is_empty()
        || operation
            .supported_cohorts
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != operation.supported_cohorts.len()
    {
        return Err(invalid_snapshot(format!(
            "operation {:?} supported cohorts must be non-empty and unique",
            operation.id
        )));
    }

    if operation.factors.is_empty()
        || operation.checks.is_empty()
        || operation.comparison.factors.is_empty()
    {
        return Err(invalid_snapshot(format!(
            "operation {:?} must declare factors, correctness checks, and a comparison projection",
            operation.id
        )));
    }
    let factor_ids = operation
        .factors
        .iter()
        .map(|factor| factor.id)
        .collect::<BTreeSet<_>>();
    if factor_ids.len() != operation.factors.len() {
        return Err(invalid_snapshot(format!(
            "operation {:?} factor ids must be unique",
            operation.id
        )));
    }
    let comparison_ids = operation
        .comparison
        .factors
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if comparison_ids.len() != operation.comparison.factors.len()
        || !comparison_ids.is_subset(&factor_ids)
    {
        return Err(invalid_snapshot(format!(
            "operation {:?} comparison projection contains duplicate or unknown factors",
            operation.id
        )));
    }
    let count_factor = match operation.count_semantics {
        CountSemantics::ConcurrentRequests { factor }
        | CountSemantics::ConcurrentWorkspaceCreates { factor } => factor,
        CountSemantics::SingleRequestWithPreparedLoad { load_factor } => load_factor,
    };
    if !factor_ids.contains(&count_factor) {
        return Err(invalid_snapshot(format!(
            "operation {:?} count semantics references an undeclared factor",
            operation.id
        )));
    }
    for factor in &operation.factors {
        if blank(&factor.label)
            || blank(&factor.help)
            || !factor_shape_is_valid(factor)
            || (factor.comparison == SnapshotComparisonParticipation::ScientificInvariant)
                != comparison_ids.contains(&factor.id)
        {
            return Err(invalid_snapshot(format!(
                "operation {:?} factor {:?} scientific metadata is inconsistent",
                operation.id, factor.id
            )));
        }
    }

    let mut check_ids = BTreeSet::new();
    for check in &operation.checks {
        if !check_ids.insert(check.id)
            || blank(&check.label)
            || blank(&check.help)
            || check.semantic_revision == 0
            || check.evidence_limit == 0
        {
            return Err(invalid_snapshot(format!(
                "operation {:?} check metadata is incomplete or duplicated",
                operation.id
            )));
        }
    }
    let mut phase_ids = BTreeSet::new();
    for phase in &operation.phases {
        if !phase_ids.insert(phase.id)
            || blank(&phase.label)
            || blank(&phase.help)
            || phase.semantic_revision == 0
            || phase.unit != PhaseUnit::Nanoseconds
            || phase.source != PhaseSource::ProductTrace
            || phase.correlation != PhaseCorrelationRule::ExactRequestTraceSpan
            || blank(&phase.trace_span_name)
        {
            return Err(invalid_snapshot(format!(
                "operation {:?} phase metadata is incomplete or duplicated",
                operation.id
            )));
        }
    }
    Ok(())
}

fn operation_identity_matches_v1(operation: &SnapshotOperation) -> bool {
    match operation.id {
        OperationId::ExecCommand => {
            operation.family == FamilyId::Command
                && operation.count_semantics
                    == (CountSemantics::ConcurrentRequests {
                        factor: FactorId::ConcurrentRequests,
                    })
                && operation.execution_shape == ExecutionShape::BarrierRequestBatch
                && operation.isolation == IsolationPolicy::SessionModeDependent
                && operation.cleanup == CleanupPolicy::ResolveFromIsolation
                && operation.product_access
                    == ProductAccess::PublicGateway(ProductOperation::ExecCommand)
                && operation.security_class == SecurityClass::BoundedShell
        }
        OperationId::FileRead => file_operation_identity_matches(
            operation,
            ProductOperation::FileRead,
            IsolationPolicy::ReusableVerifiedFixture,
            CleanupPolicy::VerifyFixtureUnchanged,
            SecurityClass::PublicReadOnly,
        ),
        OperationId::FileWrite => file_operation_identity_matches(
            operation,
            ProductOperation::FileWrite,
            IsolationPolicy::MutationDestinationDependent,
            CleanupPolicy::ResolveFromIsolation,
            SecurityClass::PublicMutation,
        ),
        OperationId::FileEdit => file_operation_identity_matches(
            operation,
            ProductOperation::FileEdit,
            IsolationPolicy::MutationDestinationDependent,
            CleanupPolicy::ResolveFromIsolation,
            SecurityClass::PublicMutation,
        ),
        OperationId::FileBlame => file_operation_identity_matches(
            operation,
            ProductOperation::FileBlame,
            IsolationPolicy::ReusableVerifiedFixture,
            CleanupPolicy::VerifyFixtureUnchanged,
            SecurityClass::PublicReadOnly,
        ),
        OperationId::CreateWorkspace => {
            operation.family == FamilyId::WorkspaceLifecycle
                && operation.count_semantics
                    == (CountSemantics::ConcurrentWorkspaceCreates {
                        factor: FactorId::WorkspaceCount,
                    })
                && operation.execution_shape == ExecutionShape::BarrierWorkspaceCreation
                && operation.isolation == IsolationPolicy::PreparedSandboxPerCell
                && operation.cleanup == CleanupPolicy::DestroySessionsAndVerifyBaseline
                && operation.product_access
                    == ProductAccess::InternalWorkspace(WorkspaceAction::CreateNoOpSession)
                && operation.security_class == SecurityClass::InternalWorkspaceLifecycle
        }
        OperationId::SquashLayerstack => {
            operation.family == FamilyId::LayerStack
                && operation.count_semantics
                    == (CountSemantics::SingleRequestWithPreparedLoad {
                        load_factor: FactorId::LiveSessions,
                    })
                && operation.execution_shape == ExecutionShape::SingleRequestAfterPreparedLoad
                && operation.isolation == IsolationPolicy::FreshTopologyPerTrial
                && operation.cleanup == CleanupPolicy::DestroyTopologyAndVerifyBaseline
                && operation.product_access
                    == ProductAccess::PublicGateway(ProductOperation::SquashLayerstacks)
                && operation.security_class == SecurityClass::DestructiveManagerMutation
        }
    }
}

fn file_operation_identity_matches(
    operation: &SnapshotOperation,
    product: ProductOperation,
    isolation: IsolationPolicy,
    cleanup: CleanupPolicy,
    security: SecurityClass,
) -> bool {
    operation.family == FamilyId::Files
        && operation.count_semantics
            == (CountSemantics::ConcurrentRequests {
                factor: FactorId::ConcurrentRequests,
            })
        && operation.execution_shape == ExecutionShape::BarrierRequestBatch
        && operation.isolation == isolation
        && operation.cleanup == cleanup
        && operation.product_access == ProductAccess::PublicGateway(product)
        && operation.security_class == security
}

fn factor_shape_is_valid(factor: &SnapshotFactorDefinition) -> bool {
    match (&factor.value_kind, factor.unit, &factor.constraint) {
        (
            SnapshotFactorValueKind::UnsignedInteger,
            Some(SnapshotFactorUnit::Count | SnapshotFactorUnit::Bytes),
            SnapshotFactorConstraint::Positive | SnapshotFactorConstraint::NonNegative,
        )
        | (
            SnapshotFactorValueKind::UnitRatio,
            Some(SnapshotFactorUnit::Ratio),
            SnapshotFactorConstraint::UnitInterval,
        )
        | (
            SnapshotFactorValueKind::Choice,
            None,
            SnapshotFactorConstraint::ProfileCatalog {
                catalog: SnapshotProfileCatalogId::WorkspaceProfiles,
            },
        ) => true,
        (SnapshotFactorValueKind::Choice, None, SnapshotFactorConstraint::Choices { values }) => {
            !values.is_empty()
                && values.iter().all(|value| !blank(value))
                && values.iter().collect::<BTreeSet<_>>().len() == values.len()
        }
        _ => false,
    }
}

fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

fn invalid_snapshot(message: impl Into<String>) -> ReportError {
    ReportError::InvalidDefinitionSnapshot(message.into())
}

fn invalid_observation(message: impl Into<String>) -> ReportError {
    ReportError::InvalidDefinitionSnapshot(format!("observation linkage: {}", message.into()))
}

fn trial_is_eligible(trial: &TrialSample) -> bool {
    trial.product_succeeded
        && !trial.infrastructure_failed
        && trial.cleanup_baseline_restored
        && trial.correctness.eligible_for_latency
}

fn correctness_failed(trial: &TrialSample) -> bool {
    trial.product_succeeded
        && !trial.infrastructure_failed
        && trial.cleanup_baseline_restored
        && correctness_fold_failed(&trial.correctness)
}

fn correctness_fold_failed(fold: &CorrectnessFold) -> bool {
    fold.failed_check_count > 0
        || !fold.missing_checks.is_empty()
        || !fold.unexpected_checks.is_empty()
}

fn ratio_scale(unit: MetricUnit) -> bool {
    matches!(
        unit,
        MetricUnit::Bytes
            | MetricUnit::BytesPerSecond
            | MetricUnit::Nanoseconds
            | MetricUnit::OperationsPerSecond
            | MetricUnit::Count
            | MetricUnit::Ratio
    )
}

fn exact_nonnegative_integer(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value <= u64::MAX as f64 && value.fract() == 0.0)
        .then_some(value as u64)
}

fn metric_copy(id: &str) -> (&'static str, &'static str) {
    match id {
        PRIMARY_LATENCY_METRIC_ID => (
            "Batch makespan",
            "Barrier release until the last issued product request reaches a terminal response.",
        ),
        REQUEST_LATENCY_METRIC_ID => (
            "Request latency",
            "One issued product request from send until its final response is decoded.",
        ),
        THROUGHPUT_METRIC_ID => (
            "Throughput",
            "Successful issued product requests divided by batch makespan seconds.",
        ),
        SETUP_METRIC_ID => (
            "Setup",
            "Harness setup time outside the primary operation window.",
        ),
        VERIFY_METRIC_ID => (
            "Verification",
            "Correctness verification time outside the primary operation window.",
        ),
        TEARDOWN_METRIC_ID => (
            "Teardown",
            "Owned cleanup and baseline verification time outside the primary operation window.",
        ),
        "runner_rss_bytes" => (
            "Runner RSS",
            "Maximum resident bytes of the benchmark runner.",
        ),
        "daemon_rss_bytes" => (
            "Daemon RSS",
            "Maximum resident bytes of the sandbox daemon.",
        ),
        "daemon_cpu_time_ns" => ("Daemon CPU time", "Daemon cumulative CPU-time delta."),
        "sandbox_memory_current_bytes" => (
            "Sandbox memory current",
            "Maximum sampled sandbox cgroup memory.current bytes.",
        ),
        "sandbox_memory_peak_bytes" => (
            "Sandbox memory peak",
            "Sandbox cgroup memory.peak, or an explicitly sampled peak when unavailable.",
        ),
        "sandbox_cpu_time_ns" => (
            "Sandbox CPU time",
            "Sandbox cumulative cgroup CPU-use counter delta over the trial window.",
        ),
        "sandbox_block_read_bytes" => (
            "Sandbox block reads",
            "Sandbox cumulative block-read byte counter delta over the trial window.",
        ),
        "sandbox_block_write_bytes" => (
            "Sandbox block writes",
            "Sandbox cumulative block-write byte counter delta over the trial window.",
        ),
        "workspace_logical_bytes" => (
            "Workspace logical bytes",
            "Maximum logical file bytes in the named workspace scope.",
        ),
        "workspace_allocated_bytes" => (
            "Workspace allocated bytes",
            "Maximum allocated filesystem bytes in the named workspace scope.",
        ),
        "workspace_file_count" => (
            "Workspace files",
            "Maximum file count in the workspace scope.",
        ),
        "layerstack_allocated_bytes" => (
            "LayerStack allocated bytes",
            "Maximum allocated bytes in the named LayerStack storage scope.",
        ),
        "upperdir_allocated_bytes" => (
            "Upperdir allocated bytes",
            "Maximum allocated bytes in the sandbox upperdir scope.",
        ),
        "host_free_bytes" => (
            "Host free space",
            "Minimum free bytes on the benchmark volume.",
        ),
        _ => (
            "Registered metric",
            "Persisted metric from the immutable definition snapshot.",
        ),
    }
}

fn family_label(snapshot: &SnapshotCatalog, id: FamilyId) -> String {
    snapshot
        .families
        .iter()
        .find(|family| family.id == id)
        .map_or_else(|| format!("{id:?}"), |family| family.label.clone())
}

fn operation_label(snapshot: &SnapshotCatalog, id: OperationId) -> String {
    snapshot
        .operations
        .iter()
        .find(|operation| operation.id == id)
        .map_or_else(|| format!("{id:?}"), |operation| operation.label.clone())
}

fn research_question(snapshot: &SnapshotCatalog, expanded: &ExpandedPlan) -> String {
    let families = expanded
        .cells
        .iter()
        .map(|cell| cell.family_id)
        .collect::<BTreeSet<_>>();
    snapshot
        .families
        .iter()
        .filter(|family| families.contains(&family.id))
        .map(|family| family.research_question.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn environment_fingerprint(manifest: &RunManifest) -> Result<String, serde_json::Error> {
    sha256_json(&(
        &manifest.environment.host,
        &manifest.environment.image_digest,
        &manifest.environment.workspace_root_identity,
        manifest.environment.client_cohort,
        &manifest.environment.gateway_endpoint_identity,
        &manifest.fixed_lifecycle_policy,
        &manifest.failure_policy,
        &manifest.effective_timeouts,
        &manifest.gateway_policy,
        &manifest.safety_policy,
    ))
}

fn methods(
    manifest: &RunManifest,
    expanded: &ExpandedPlan,
    design_counts: ReportDesignCounts,
) -> MethodsReport {
    MethodsReport {
        schema_version: 1,
        report_derivation_revision: REPORT_DERIVATION_REVISION,
        artifact_reader_revision: 1,
        plan_schema_version: expanded.schema_version,
        plan_seed: expanded.canonical_plan.seed,
        cell_order: expanded.canonical_plan.protocol.order,
        resource_sample_interval_ms: expanded.canonical_plan.protocol.resource_interval_ms,
        design_counts,
        fixture_generator_revision: manifest.fixture_generator_revision,
        fixture_hashes: manifest.fixture_hashes.clone(),
        producer: manifest.producer.clone(),
        artifact_schemas: manifest.artifact_schemas.clone(),
        operation_authorities: manifest.operation_authorities.clone(),
        metric_revisions: manifest.metric_revisions.clone(),
        derived_metric_revisions: [
            PRIMARY_LATENCY_METRIC_ID,
            REQUEST_LATENCY_METRIC_ID,
            THROUGHPUT_METRIC_ID,
            SETUP_METRIC_ID,
            VERIFY_METRIC_ID,
            TEARDOWN_METRIC_ID,
        ]
        .into_iter()
        .map(|metric_id| crate::scheduler::MetricRevisionIdentity {
            metric_id: metric_id.to_owned(),
            semantic_revision: PRIMARY_LATENCY_METRIC_REVISION,
        })
        .collect(),
        check_revisions: manifest.check_revisions.clone(),
        phase_revisions: manifest.phase_revisions.clone(),
        environment: manifest.environment.clone(),
        raw_time_unit: "integer_nanoseconds".to_owned(),
        monotonic_clock: manifest.environment.host.monotonic_clock.clone(),
        quantile_interpolation: "linear_type_7_v1".to_owned(),
        confidence_interval: "deterministic_percentile_bootstrap_median_95_percent".to_owned(),
        bootstrap_resamples: statistics::BOOTSTRAP_RESAMPLES,
        outlier_policy: "tukey_1_5_iqr_flagged_and_retained".to_owned(),
        warmup_policy: "recorded_but_excluded_from_statistics".to_owned(),
        failure_policy:
            "only measured product-success, required-check-pass, cleanup-restored trials are eligible"
                .to_owned(),
        resource_policy:
            "availability is explicit; unavailable samples are never converted to zero".to_owned(),
        comparison_policy:
            "compatibility_before_delta; independent bootstrap; no p-value or v1 regression verdict"
                .to_owned(),
    }
}

fn limitations() -> Vec<String> {
    vec![
        "P95 is exploratory below 20 successful observations; P99 is not computed in v1."
            .to_owned(),
        "Median confidence intervals are omitted below five successful observations.".to_owned(),
        "Unavailable resource evidence remains unavailable and cannot be interpreted as zero."
            .to_owned(),
        "V1 reports direction metadata but makes no automatic regression or significance claim."
            .to_owned(),
    ]
}

fn bootstrap_seed(run_seed: u64, cell_id: &str, metric_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(run_seed.to_le_bytes());
    hasher.update(cell_id.as_bytes());
    hasher.update(metric_id.as_bytes());
    hasher.update(REPORT_DERIVATION_REVISION.to_le_bytes());
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_vec(value).map(|bytes| sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
