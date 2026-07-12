use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::ExecutionDependencies;
use crate::artifacts::{
    ArtifactError, ArtifactId, ArtifactRef, ArtifactStore, BOUNDED_EVIDENCE_SCHEMA_NAME,
    BOUNDED_EVIDENCE_SCHEMA_VERSION,
};
use crate::checks::{fold_correctness, CheckResult, CheckVerdict, CorrectnessFold};
use crate::cleanup::{CleanupError, CleanupLedger, OwnedIdentity};
use crate::config::StartupConfig;
use crate::definitions::DefinitionCatalog;
use crate::events::{
    EventData, EventError, EventJournal, LifecyclePhase, LogLevel, RequestState, RunState,
    WorkState, EVENT_SCHEMA_NAME, EVENT_SCHEMA_VERSION,
};
use crate::executors::{
    ExecutorError, OperationOutcome, ProductOutputStatus, RuntimeContext, RuntimeInvocation,
    Verification,
};
use crate::fixtures::{self, FixtureError, MaterializedFixture};
use crate::gateway::{
    Correlation, GatewayError, GatewayLaunchConfig, IsolatedGateway, OwnedSandboxId,
    ProductGateway, ProductLayerstackSnapshot, ProductSandboxResources, ProductStorageResources,
    ProductTrace, ProductTraceSpanNode, ProductTraceStatus, ProductUpperdirAllocation,
    LOG_DRAIN_TIMEOUT, MAX_COMMAND_TIMEOUT_MS, MAX_GATEWAY_CONNECTIONS, MAX_LOG_BYTES,
    MAX_LOG_LINE_BYTES, MAX_PRODUCT_CONTENT_BYTES, MAX_PRODUCT_EDITS, MAX_PRODUCT_PATH_BYTES,
    MAX_PRODUCT_RESOURCE_SAMPLES, MAX_PRODUCT_TRACE_NODES, OWNED_RESOURCE_CLEANUP_TIMEOUT,
    PRODUCT_RESOURCE_WINDOW_MS, READINESS_POLL, READINESS_PROBE_TIMEOUT, READINESS_TIMEOUT,
    SHUTDOWN_TIMEOUT,
};
use crate::model::{
    CheckId, CleanupPolicy, ClientCohort, CountSemantics, ExpandedOperationCell, ExperimentPlan,
    FamilyId, OperationEvidence, OperationId, PhaseCorrelationRule, PhaseId, PhaseSource,
    PhaseUnit, ProductAccess, ResolvedIsolationPolicy, WorkspaceProfileId,
};
use crate::plan::{
    effective_environment, ExpandedCell, ExpandedPlan, FixedLifecyclePolicy, GatewayMode,
    PresetRef, RuntimeEnvironmentSnapshot, MAX_EXPANDED_CELLS, MAX_ISSUED_OPERATION_REQUEST_COUNT,
    MAX_TRIAL_BATCH_COUNT,
};
use crate::report::{self, ReportError};
use crate::resources::{
    parse_df_available_bytes, Availability, HostVolumeCollector, MetricCollector, MetricDefinition,
    MetricSamplingTask, MonotonicInstant, ProcessCollector, ProcessScope, ResourceReading,
    SamplingInterval, VolumeCollector, VolumeScope, DAEMON_CPU_TIME, DAEMON_RSS, LAYERSTACK_BYTES,
    SANDBOX_BLOCK_READ, SANDBOX_BLOCK_WRITE, SANDBOX_CPU_TIME, SANDBOX_MEMORY_CURRENT,
    SANDBOX_MEMORY_PEAK, UPPERDIR_BYTES,
};
use crate::{daemon_session::WorkspaceSessionAdapter, executors};

pub const RUN_MANIFEST_SCHEMA_NAME: &str = "eos_benchmark_run_manifest";
// Manifest v1 has not shipped. These fields complete its first release shape;
// after release, changes require a new version plus an explicit v1 decoder.
pub const RUN_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const OBSERVATION_SCHEMA_NAME: &str = "eos_benchmark_observation";
pub const OBSERVATION_SCHEMA_VERSION: u32 = 3;
pub const DEFINITION_SNAPSHOT_SCHEMA_NAME: &str = "eos_benchmark_definition_snapshot";
pub const ENVIRONMENT_METADATA_SCHEMA_NAME: &str = "eos_benchmark_environment_metadata";
pub const INTENT_PLAN_SCHEMA_NAME: &str = "eos_benchmark_intent_plan";
pub const EXPANDED_PLAN_SCHEMA_NAME: &str = "eos_benchmark_expanded_plan";
const ENVIRONMENT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ENVIRONMENT_PROBE_BYTES: usize = 1024 * 1024;
const MAX_SCHEDULER_EVENT_TEXT_BYTES: usize = 4_096;
// Gateway stdout/stderr are already line-redacted and capped by `LogCapture`.
// Retain them in the resumable diagnostic event artifact in chunks that leave
// ample room for the fixed, versioned chunk header below EventJournal's cap.
const GATEWAY_LOG_EVENT_PAYLOAD_BYTES: usize = 3_000;
const MAX_FAILURE_CODE_BYTES: usize = 128;
const SANDBOX_CREATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SANDBOX_DESTROY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const OPERATION_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const GATEWAY_STOP_TIMEOUT: Duration = Duration::from_secs(12 * 60);
const DEFAULT_CANCELLATION_GRACE: Duration = Duration::from_secs(2);
const LAYERSTACK_CANCELLATION_GRACE: Duration = Duration::from_secs(5);
const TRIAL_DIRECTORY_DIGEST_BYTES: usize = 16;
const EMERGENCY_EVENT_OFFSET_NS: u64 = u64::MAX;
const LAYERSTACK_REQUEST_ID: &str = "squash-layerstack-0";
const LAYERSTACK_OBSERVATION_SOURCE: &str = "product_observability.layerstack";
const LAYERSTACK_TRACE_SOURCE: &str = "product_observability.trace";
const LAYERSTACK_SETTLE_INTERVAL: Duration = Duration::from_millis(100);
const LAYERSTACK_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const LAYERSTACK_SETTLE_MATCHES: usize = 3;
const LAYERSTACK_TRACE_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const LAYERSTACK_TRACE_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const LAYERSTACK_OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);
const PRODUCT_RESOURCE_OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);
const PRODUCT_RESOURCE_SOURCE: &str = "product_observability.cgroup.docker_engine";
const PRODUCT_STORAGE_SOURCE: &str = "product_observability.snapshot";
const PREPARED_CELL_LIFECYCLE_ID: &str = "prepared-cell";

/// Closed cancellation boundaries for scheduler-owned asynchronous work.
///
/// A cancellation request durably records `cancelling` before it signals the
/// campaign token. The boundary then gives already-started owned work a short,
/// operation-specific grace period before the future is dropped and
/// identity-checked teardown takes over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellationBoundary {
    SandboxCreate,
    OperationPrepare(OperationId),
    OperationRequest(OperationId),
    OperationVerify(OperationId),
}

impl CancellationBoundary {
    const fn grace(self) -> Duration {
        match self {
            Self::SandboxCreate => DEFAULT_CANCELLATION_GRACE,
            Self::OperationPrepare(operation)
            | Self::OperationRequest(operation)
            | Self::OperationVerify(operation) => match operation {
                OperationId::ExecCommand
                | OperationId::FileRead
                | OperationId::FileWrite
                | OperationId::FileEdit
                | OperationId::FileBlame
                | OperationId::CreateWorkspace => DEFAULT_CANCELLATION_GRACE,
                OperationId::SquashLayerstack => LAYERSTACK_CANCELLATION_GRACE,
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OwnedTaskOutcome<T> {
    Completed(T),
    TimedOut,
    CancelledBeforeStart,
    CancelledCompleted(T),
    CancelledAfterGrace,
}

async fn await_owned_task<F>(
    cancellation: &CancellationToken,
    timeout: Option<Duration>,
    boundary: CancellationBoundary,
    future: F,
) -> OwnedTaskOutcome<F::Output>
where
    F: Future,
{
    await_owned_task_with_grace(cancellation, timeout, boundary.grace(), future).await
}

async fn await_owned_task_with_grace<F>(
    cancellation: &CancellationToken,
    timeout: Option<Duration>,
    cancellation_grace: Duration,
    future: F,
) -> OwnedTaskOutcome<F::Output>
where
    F: Future,
{
    // Async functions are lazy. Track the first poll so a cancellation that
    // wins before work starts drops the future instead of polling it during
    // the grace period and accidentally issuing a new product request.
    let started = AtomicBool::new(false);
    let tracked = async {
        started.store(true, Ordering::Relaxed);
        future.await
    };
    tokio::pin!(tracked);

    match timeout {
        Some(timeout) => {
            let deadline = tokio::time::sleep(timeout);
            tokio::pin!(deadline);
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    if started.load(Ordering::Relaxed) {
                        match tokio::time::timeout(cancellation_grace, &mut tracked).await {
                            Ok(output) => OwnedTaskOutcome::CancelledCompleted(output),
                            Err(_) => OwnedTaskOutcome::CancelledAfterGrace,
                        }
                    } else {
                        OwnedTaskOutcome::CancelledBeforeStart
                    }
                },
                output = &mut tracked => OwnedTaskOutcome::Completed(output),
                () = &mut deadline => OwnedTaskOutcome::TimedOut,
            }
        }
        None => {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    if started.load(Ordering::Relaxed) {
                        match tokio::time::timeout(cancellation_grace, &mut tracked).await {
                            Ok(output) => OwnedTaskOutcome::CancelledCompleted(output),
                            Err(_) => OwnedTaskOutcome::CancelledAfterGrace,
                        }
                    } else {
                        OwnedTaskOutcome::CancelledBeforeStart
                    }
                },
                output = &mut tracked => OwnedTaskOutcome::Completed(output),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreatmentIdentity {
    pub source_commit: String,
    pub source_dirty: bool,
    pub source_diff_hash: Option<String>,
    pub daemon_binary_hash: Option<String>,
    pub gateway_binary_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEnvironment {
    pub operating_system: String,
    pub architecture: String,
    pub kernel_release: Option<String>,
    pub docker_engine_version: Option<String>,
    pub filesystem: Option<String>,
    pub free_space_bytes: Option<u64>,
    pub monotonic_clock: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentMetadata {
    pub schema_version: u32,
    pub treatment: TreatmentIdentity,
    pub host: HostEnvironment,
    pub image_reference: String,
    pub image_digest: Option<String>,
    pub workspace_root_identity: String,
    pub client_cohort: ClientCohort,
    pub gateway_endpoint_identity: String,
}

#[derive(Debug)]
pub struct CapturedRunEnvironment {
    pub runtime: RuntimeEnvironmentSnapshot,
    pub metadata: EnvironmentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFailure {
    pub code: String,
    pub message: String,
    pub infrastructure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerIdentity {
    pub package: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSchemaIdentity {
    pub schema_name: String,
    pub write_version: u32,
    pub read_versions: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSchemaSet {
    pub run_manifest: ArtifactSchemaIdentity,
    pub intent_plan: ArtifactSchemaIdentity,
    pub expanded_plan: ArtifactSchemaIdentity,
    pub definition_snapshot: ArtifactSchemaIdentity,
    pub environment_metadata: ArtifactSchemaIdentity,
    pub events: ArtifactSchemaIdentity,
    pub observations: ArtifactSchemaIdentity,
    pub bounded_evidence: ArtifactSchemaIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionSnapshotIdentity {
    pub schema_name: String,
    pub schema_version: u32,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricRevisionIdentity {
    pub metric_id: String,
    pub semantic_revision: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRevisionIdentity {
    pub check_id: CheckId,
    pub semantic_revision: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseRevisionIdentity {
    pub phase_id: PhaseId,
    pub semantic_revision: u32,
}

#[derive(Debug)]
struct ManifestRevisionIdentities {
    metrics: Vec<MetricRevisionIdentity>,
    checks: Vec<CheckRevisionIdentity>,
    phases: Vec<PhaseRevisionIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StabilizationPolicy {
    NotRequired {
        semantic_revision: u32,
    },
    ExactSnapshotQuietWindow {
        semantic_revision: u32,
        quiet_window_matches: u32,
        poll_interval_ms: u64,
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationAuthority {
    pub operation_id: OperationId,
    pub family_id: FamilyId,
    pub semantic_revision: u32,
    pub factor_schema_revision: u32,
    pub comparison_projection_revision: u32,
    pub client_cohort: ClientCohort,
    pub product_access: ProductAccess,
    pub count_semantics: CountSemantics,
    pub cleanup_policy: CleanupPolicy,
    pub resolved_isolation_policies: Vec<ResolvedIsolationPolicy>,
    pub request_timeout_ms: Vec<u64>,
    pub stabilization_policy: StabilizationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureAction {
    ContinueWhenEnvironmentSafe,
    AbortCampaign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveFailurePolicy {
    pub semantic_revision: u32,
    pub product_transport_timeout_or_correctness: FailureAction,
    pub fixture_containment_ownership_environment_or_infrastructure: FailureAction,
    pub teardown_or_cleanup_baseline: FailureAction,
    pub automatic_measured_operation_retries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveTimeoutPolicy {
    pub sandbox_create_timeout_ms: u64,
    pub sandbox_destroy_timeout_ms: u64,
    pub operation_teardown_timeout_ms: u64,
    pub gateway_stop_timeout_ms: u64,
    pub gateway_owned_resource_cleanup_timeout_ms: u64,
    pub gateway_shutdown_timeout_ms: u64,
    pub gateway_log_drain_timeout_ms: u64,
    pub layerstack_observation_timeout_ms: u64,
    pub layerstack_trace_retry_timeout_ms: u64,
    pub product_resource_observation_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveGatewayPolicy {
    pub semantic_revision: u32,
    pub mode: GatewayMode,
    pub loopback_only: bool,
    pub isolated_runtime_per_execution_block: bool,
    pub remount_sweep_widths: Vec<u32>,
    pub maximum_connections: u64,
    pub readiness_timeout_ms: u64,
    pub readiness_probe_timeout_ms: u64,
    pub readiness_poll_interval_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductCapId {
    StoredCommandOutput,
    FileReadReturnedBytes,
    FileWriteContentBytes,
    FileEditFileBytes,
    FileEditReplacementCount,
    FileBlameAuditabilityEvents,
    LayerstackPreparedLayers,
    LayerstackPayloadBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapUnit {
    Count,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapBehavior {
    RejectAboveMaximum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapResolution {
    pub requested: u64,
    pub effective: u64,
    pub fixed_maximum: u64,
    pub unit: CapUnit,
    pub cap_revision: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignCapResolutions {
    pub expanded_test_combinations: CapResolution,
    pub trial_batches: CapResolution,
    pub issued_product_requests: CapResolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCapResolution {
    pub operation_id: OperationId,
    pub cap_id: ProductCapId,
    pub maximum_requested: u64,
    pub maximum_effective: u64,
    pub fixed_maximum: u64,
    pub unit: CapUnit,
    pub cap_revision: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedGatewaySafetyCaps {
    pub log_bytes: u64,
    pub log_line_bytes: u64,
    pub product_path_bytes: u64,
    pub product_content_bytes: u64,
    pub product_edits: u64,
    pub command_timeout_ms: u64,
    pub product_trace_nodes: u64,
    pub product_resource_window_ms: u64,
    pub product_resource_samples: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveSafetyPolicy {
    pub semantic_revision: u32,
    pub cap_behavior: CapBehavior,
    pub campaign_caps: CampaignCapResolutions,
    pub product_caps: Vec<ProductCapResolution>,
    pub fixed_gateway_caps: FixedGatewaySafetyCaps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunManifest {
    pub schema_version: u32,
    pub run_id: String,
    pub name: String,
    pub plan_hash: String,
    pub starting_preset: Option<PresetRef>,
    pub state: RunState,
    pub producer: ProducerIdentity,
    pub artifact_schemas: ArtifactSchemaSet,
    pub definition_snapshot: DefinitionSnapshotIdentity,
    pub operation_authorities: Vec<OperationAuthority>,
    pub metric_revisions: Vec<MetricRevisionIdentity>,
    pub check_revisions: Vec<CheckRevisionIdentity>,
    pub phase_revisions: Vec<PhaseRevisionIdentity>,
    pub fixed_lifecycle_policy: FixedLifecyclePolicy,
    pub failure_policy: EffectiveFailurePolicy,
    pub effective_timeouts: EffectiveTimeoutPolicy,
    pub gateway_policy: EffectiveGatewayPolicy,
    pub safety_policy: EffectiveSafetyPolicy,
    pub treatment: TreatmentIdentity,
    pub environment: EnvironmentMetadata,
    pub fixture_generator_revision: u32,
    pub fixture_hashes: BTreeMap<String, String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub failure: Option<RunFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialKind {
    Warmup,
    Measured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleDurations {
    pub setup_ns: u64,
    pub operation_ns: u64,
    pub verify_ns: u64,
    pub teardown_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialSample {
    pub operation_id: OperationId,
    pub cell_id: String,
    pub trial_id: String,
    pub kind: TrialKind,
    pub sequence_in_cell: u32,
    pub lifecycle: LifecycleDurations,
    pub product_succeeded: bool,
    pub infrastructure_failed: bool,
    pub cleanup_baseline_restored: bool,
    pub correctness: CorrectnessFold,
    pub primary_operation_latency_ns: Option<u64>,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestObservation {
    pub operation_id: OperationId,
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: String,
    pub start_offset_ns: u64,
    pub latency_ns: u64,
    pub succeeded: bool,
    pub status: String,
    pub response_bytes: u64,
    pub bounded_response_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObservation {
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: Option<String>,
    pub reading: ResourceReading,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseObservation {
    pub id: PhaseId,
    pub semantic_revision: u32,
    pub unit: PhaseUnit,
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: Option<String>,
    pub source: PhaseSource,
    pub correlation: PhaseCorrelationRule,
    pub trace_span_name: String,
    pub start_offset_ns: u64,
    pub duration_ns: u64,
    pub status: PhaseStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationObservation {
    pub operation_id: OperationId,
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: Option<String>,
    pub evidence: OperationEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", content = "data", rename_all = "snake_case")]
pub enum ObservationRecord {
    Trial(TrialSample),
    Request(RequestObservation),
    Resource(ResourceObservation),
    Phase(PhaseObservation),
    Check(CheckResult),
    Operation(OperationObservation),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequencedObservation {
    pub sequence: u64,
    pub record: ObservationRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProgress {
    pub current_family: Option<crate::model::FamilyId>,
    pub current_operation: Option<OperationId>,
    pub current_cell_id: Option<String>,
    pub current_trial_id: Option<String>,
    pub trial_kind: Option<TrialKind>,
    pub phase: Option<crate::events::LifecyclePhase>,
    pub completed_trial_batches: u64,
    pub total_trial_batches: u64,
    pub issued_operation_requests: u64,
    pub warning_count: u64,
    pub failure_count: u64,
}

#[derive(Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

#[derive(Debug)]
pub struct ActiveCampaign {
    pub run_id: String,
    pub state: RunState,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Default)]
pub struct CampaignGate {
    state: Mutex<CampaignGateState>,
}

#[derive(Debug, Default)]
struct CampaignGateState {
    active: Option<ActiveCampaign>,
    idempotency: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum CampaignGateError {
    #[error("another campaign is active: {run_id}")]
    Busy { run_id: String },
    #[error("client request id must be non-empty and at most 128 bytes")]
    InvalidClientRequestId,
    #[error("run {0} is not active")]
    NotActive(String),
    #[error("client request id {client_request_id} is not reserved for run {run_id}")]
    ReservationMismatch {
        client_request_id: String,
        run_id: String,
    },
    #[error("campaign gate lock was poisoned")]
    Poisoned,
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("run manifest lock was poisoned")]
    ManifestPoisoned,
    #[error("run manifest is terminal and immutable")]
    TerminalManifest,
    #[error("invalid run manifest transition from {from:?} to {to:?}")]
    InvalidManifestTransition { from: RunState, to: RunState },
    #[error("timestamp formatting failed: {0}")]
    Timestamp(#[from] time::error::Format),
    #[error("artifact task failed: {0}")]
    ArtifactTask(String),
    #[error("environment probe failed while {action}: {source}")]
    EnvironmentIo {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("environment probe timed out while {0}")]
    EnvironmentTimeout(&'static str),
    #[error("environment probe exited unsuccessfully while {0}")]
    EnvironmentProbe(&'static str),
    #[error("environment probe output exceeded its fixed cap while {0}")]
    EnvironmentOutputCap(&'static str),
    #[error("environment probe returned invalid UTF-8 while {0}")]
    EnvironmentUtf8(&'static str),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    Cleanup(#[from] CleanupError),
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("campaign plan is not runnable: {0}")]
    InvalidPlan(String),
    #[error("run manifest authority is inconsistent: {0}")]
    InvalidManifestAuthority(String),
    #[error("campaign execution block is inconsistent: {0}")]
    InvalidExecutionBlock(String),
    #[error("client cohort {0:?} has no installed closed execution adapter")]
    UnsupportedClientCohort(ClientCohort),
    #[error("resource sampling interval is invalid: {0} ms")]
    InvalidResourceInterval(u64),
    #[error("campaign filesystem operation failed for {path}: {source}")]
    CampaignIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("campaign worker failed: {0}")]
    CampaignTask(String),
    #[error("campaign resource collection failed: {0}")]
    ResourceTask(String),
    #[error(
        "operation {operation:?} prepared {actual} invocations; expanded plan requires {expected}"
    )]
    InvocationCountMismatch {
        operation: OperationId,
        expected: u32,
        actual: usize,
    },
    #[error("trial cleanup baseline was not restored for {trial_id}: {detail}")]
    CleanupBaseline { trial_id: String, detail: String },
    #[error("gateway shutdown exceeded the fixed deadline")]
    GatewayStopTimeout,
    #[error("sandbox creation exceeded the fixed deadline for trial {0}")]
    SandboxCreateTimeout(String),
    #[error("sandbox destruction exceeded the fixed deadline for trial {0}")]
    SandboxDestroyTimeout(String),
    #[error("operation teardown exceeded the fixed deadline for trial {0}")]
    OperationTeardownTimeout(String),
}

#[derive(Debug)]
struct ProbeCapture {
    retained: Vec<u8>,
    bytes: u64,
    sha256: String,
    truncated: bool,
}

#[derive(Debug)]
pub struct RunArtifacts {
    pub run_id: String,
    pub events: Arc<EventJournal>,
    store: ArtifactStore,
    manifest: Mutex<RunManifest>,
    observation_sequence: tokio::sync::Mutex<u64>,
}

impl MonotonicClock {
    #[must_use]
    pub fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    #[must_use]
    pub fn offset_ns(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Captures run-start facts using only fixed, internally selected probes. Raw
/// command output and environment variables are never persisted.
pub async fn capture_runtime_environment(
    startup: &StartupConfig,
    dependencies: &ExecutionDependencies,
    plan: &ExperimentPlan,
) -> Result<RuntimeEnvironmentSnapshot, SchedulerError> {
    let image_digest = probe_text(
        &dependencies.docker_binary,
        &[
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            plan.environment.image.0.as_str(),
        ],
        &startup.repo,
        "resolving benchmark image",
    )
    .await?;
    validate_environment_sha256_identity(&image_digest, "validating benchmark image identity")?;

    let (filesystem, free_space_bytes) = tokio::join!(
        filesystem_probe(startup, &dependencies.stat_binary),
        free_space_probe(startup, &dependencies.df_binary)
    );
    Ok(RuntimeEnvironmentSnapshot {
        image_digest,
        filesystem,
        free_space_bytes,
    })
}

pub async fn capture_environment(
    startup: &StartupConfig,
    dependencies: &ExecutionDependencies,
    plan: &ExperimentPlan,
) -> Result<CapturedRunEnvironment, SchedulerError> {
    let runtime = capture_runtime_environment(startup, dependencies, plan).await?;
    let source_commit = probe_text(
        &dependencies.git_binary,
        &["rev-parse", "--verify", "HEAD"],
        &startup.repo,
        "reading source commit",
    )
    .await?;
    if source_commit.len() != 40 || !source_commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SchedulerError::EnvironmentProbe("validating source commit"));
    }
    let status = run_probe(
        &dependencies.git_binary,
        [
            OsString::from("status"),
            OsString::from("--porcelain=v1"),
            OsString::from("--untracked-files=normal"),
        ],
        &startup.repo,
        "reading source status",
        0,
    )
    .await?;
    let source_dirty = status.bytes > 0;
    let source_diff_hash = if source_dirty {
        let diff = run_probe(
            &dependencies.git_binary,
            [
                OsString::from("diff"),
                OsString::from("--binary"),
                OsString::from("HEAD"),
                OsString::from("--"),
            ],
            &startup.repo,
            "hashing source diff",
            0,
        )
        .await?;
        let material = format!(
            "{}:{}\n{}:{}",
            status.bytes, status.sha256, diff.bytes, diff.sha256
        );
        Some(format!("sha256:{:x}", Sha256::digest(material.as_bytes())))
    } else {
        None
    };

    let gateway_binary = dependencies.gateway_binary.clone();
    let daemon_binary = dependencies.daemon_binary.clone();
    let (gateway_binary_hash, daemon_binary_hash) = tokio::try_join!(
        hash_file(gateway_binary, "hashing gateway binary"),
        hash_file(daemon_binary, "hashing daemon binary")
    )?;
    let kernel_release = optional_system_probe(
        system_program(&["/usr/bin/uname", "/bin/uname"]),
        &[OsString::from("-r")],
        &startup.repo,
        "reading kernel release",
    )
    .await;
    let effective = effective_environment(&startup.paths, plan, Some(&runtime));

    let metadata = EnvironmentMetadata {
        schema_version: 1,
        treatment: TreatmentIdentity {
            source_commit,
            source_dirty,
            source_diff_hash,
            daemon_binary_hash: Some(daemon_binary_hash),
            gateway_binary_hash: Some(gateway_binary_hash),
        },
        host: HostEnvironment {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            kernel_release,
            docker_engine_version: Some(dependencies.docker_engine_version.clone()),
            filesystem: effective.filesystem.clone(),
            free_space_bytes: effective.free_space_bytes,
            monotonic_clock: "std::time::Instant".to_owned(),
        },
        image_reference: plan.environment.image.0.clone(),
        image_digest: effective.image_digest.clone(),
        workspace_root_identity: effective.workspace_root_identity,
        client_cohort: effective.client_cohort,
        gateway_endpoint_identity: "isolated_loopback_per_execution_block".to_owned(),
    };
    Ok(CapturedRunEnvironment { runtime, metadata })
}

async fn filesystem_probe(startup: &StartupConfig, program: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    let arguments = vec![
        OsString::from("-f"),
        OsString::from("%T"),
        startup.paths.root.as_os_str().to_owned(),
    ];
    #[cfg(not(target_os = "macos"))]
    let arguments = vec![
        OsString::from("-f"),
        OsString::from("-c"),
        OsString::from("%T"),
        startup.paths.root.as_os_str().to_owned(),
    ];
    optional_system_probe(
        Some(program.to_owned()),
        &arguments,
        &startup.repo,
        "reading workspace filesystem",
    )
    .await
}

async fn free_space_probe(startup: &StartupConfig, program: &Path) -> Option<u64> {
    let capture = run_probe(
        program,
        [
            OsString::from("-Pk"),
            startup.paths.root.as_os_str().to_owned(),
        ],
        &startup.repo,
        "reading workspace free space",
        MAX_ENVIRONMENT_PROBE_BYTES,
    )
    .await
    .ok()?;
    if capture.truncated {
        return None;
    }
    parse_df_available_bytes(&capture.retained).ok()
}

fn validate_environment_sha256_identity(
    value: &str,
    action: &'static str,
) -> Result<(), SchedulerError> {
    let digest = value
        .strip_prefix("sha256:")
        .ok_or(SchedulerError::EnvironmentProbe(action))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SchedulerError::EnvironmentProbe(action));
    }
    Ok(())
}

fn system_program(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

async fn optional_system_probe(
    program: Option<PathBuf>,
    arguments: &[OsString],
    current_directory: &Path,
    action: &'static str,
) -> Option<String> {
    let program = program?;
    let capture = run_probe(
        &program,
        arguments.iter().cloned(),
        current_directory,
        action,
        MAX_ENVIRONMENT_PROBE_BYTES,
    )
    .await
    .ok()?;
    if capture.truncated {
        return None;
    }
    String::from_utf8(capture.retained)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn probe_text(
    program: &Path,
    arguments: &[&str],
    current_directory: &Path,
    action: &'static str,
) -> Result<String, SchedulerError> {
    let capture = run_probe(
        program,
        arguments.iter().map(OsString::from),
        current_directory,
        action,
        MAX_ENVIRONMENT_PROBE_BYTES,
    )
    .await?;
    if capture.truncated {
        return Err(SchedulerError::EnvironmentOutputCap(action));
    }
    String::from_utf8(capture.retained)
        .map(|value| value.trim().to_owned())
        .map_err(|_| SchedulerError::EnvironmentUtf8(action))
}

async fn run_probe(
    program: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    current_directory: &Path,
    action: &'static str,
    retain_limit: usize,
) -> Result<ProbeCapture, SchedulerError> {
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(current_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
    let stdout = child
        .stdout
        .take()
        .ok_or(SchedulerError::EnvironmentProbe(action))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(SchedulerError::EnvironmentProbe(action))?;
    let stdout_task = tokio::spawn(drain_probe_stream(stdout, retain_limit));
    let stderr_task = tokio::spawn(drain_probe_stream(stderr, MAX_ENVIRONMENT_PROBE_BYTES));
    let timed_out = match tokio::time::timeout(ENVIRONMENT_PROBE_TIMEOUT, child.wait()).await {
        Ok(status) => {
            let status =
                status.map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
            if !status.success() {
                return Err(SchedulerError::EnvironmentProbe(action));
            }
            false
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            true
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|_| SchedulerError::EnvironmentProbe(action))?
        .map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
    let stderr = stderr_task
        .await
        .map_err(|_| SchedulerError::EnvironmentProbe(action))?
        .map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
    if timed_out {
        return Err(SchedulerError::EnvironmentTimeout(action));
    }
    if stderr.truncated {
        return Err(SchedulerError::EnvironmentOutputCap(action));
    }
    Ok(stdout)
}

async fn drain_probe_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    retain_limit: usize,
) -> io::Result<ProbeCapture> {
    let mut retained = Vec::new();
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut truncated = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        let remaining = retain_limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(ProbeCapture {
        retained,
        bytes,
        sha256: format!("sha256:{:x}", digest.finalize()),
        truncated,
    })
}

async fn hash_file(path: PathBuf, action: &'static str) -> Result<String, SchedulerError> {
    tokio::task::spawn_blocking(move || {
        let mut file =
            File::open(&path).map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| SchedulerError::EnvironmentIo { action, source })?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(format!("sha256:{:x}", digest.finalize()))
    })
    .await
    .map_err(|error| SchedulerError::ArtifactTask(error.to_string()))?
}

impl CampaignGate {
    pub fn reserve(
        &self,
        client_request_id: &str,
    ) -> Result<(String, CancellationToken, bool), CampaignGateError> {
        if client_request_id.is_empty() || client_request_id.len() > 128 {
            return Err(CampaignGateError::InvalidClientRequestId);
        }
        let mut state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        if let Some(run_id) = state.idempotency.get(client_request_id) {
            let cancellation = state
                .active
                .as_ref()
                .filter(|active| &active.run_id == run_id)
                .map_or_else(CancellationToken::new, |active| active.cancellation.clone());
            return Ok((run_id.clone(), cancellation, true));
        }
        if let Some(active) = &state.active {
            return Err(CampaignGateError::Busy {
                run_id: active.run_id.clone(),
            });
        }
        let run_id = Uuid::now_v7().to_string();
        let cancellation = CancellationToken::new();
        state
            .idempotency
            .insert(client_request_id.to_owned(), run_id.clone());
        state.active = Some(ActiveCampaign {
            run_id: run_id.clone(),
            state: RunState::Planned,
            cancellation: cancellation.clone(),
        });
        Ok((run_id, cancellation, false))
    }

    pub fn update_state(
        &self,
        run_id: &str,
        state_value: RunState,
    ) -> Result<(), CampaignGateError> {
        let mut state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        let active = state
            .active
            .as_mut()
            .filter(|active| active.run_id == run_id)
            .ok_or_else(|| CampaignGateError::NotActive(run_id.to_owned()))?;
        active.state = state_value;
        Ok(())
    }

    /// Returns the concrete cancellation token owned by the active campaign.
    /// The caller must durably record `cancelling` before signalling it.
    pub fn cancellation_token(&self, run_id: &str) -> Result<CancellationToken, CampaignGateError> {
        let state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        state
            .active
            .as_ref()
            .filter(|active| active.run_id == run_id)
            .map(|active| active.cancellation.clone())
            .ok_or_else(|| CampaignGateError::NotActive(run_id.to_owned()))
    }

    pub fn release(&self, run_id: &str) -> Result<(), CampaignGateError> {
        let mut state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        if state.active.as_ref().map(|active| active.run_id.as_str()) != Some(run_id) {
            return Err(CampaignGateError::NotActive(run_id.to_owned()));
        }
        state.active = None;
        Ok(())
    }

    /// Reverts a reservation only while it is still the active planned run.
    /// `planned` is the admission/artifact-construction boundary; once the
    /// durable manifest enters `queued`, the scheduler owns terminalization.
    pub fn rollback_reservation(
        &self,
        client_request_id: &str,
        run_id: &str,
    ) -> Result<(), CampaignGateError> {
        let mut state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        let active_matches = state
            .active
            .as_ref()
            .is_some_and(|active| active.run_id == run_id && active.state == RunState::Planned);
        let request_matches = state
            .idempotency
            .get(client_request_id)
            .is_some_and(|reserved| reserved == run_id);
        if !active_matches || !request_matches {
            return Err(CampaignGateError::ReservationMismatch {
                client_request_id: client_request_id.to_owned(),
                run_id: run_id.to_owned(),
            });
        }
        state.active = None;
        state.idempotency.remove(client_request_id);
        Ok(())
    }

    pub fn active(&self) -> Result<Option<(String, RunState)>, CampaignGateError> {
        let state = self.state.lock().map_err(|_| CampaignGateError::Poisoned)?;
        Ok(state
            .active
            .as_ref()
            .map(|active| (active.run_id.clone(), active.state)))
    }
}

impl RunManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn planned(
        run_id: &str,
        expanded: &ExpandedPlan,
        starting_preset: Option<PresetRef>,
        environment: EnvironmentMetadata,
        definition_snapshot: &DefinitionCatalog,
        definition_snapshot_sha256: String,
    ) -> Result<Self, SchedulerError> {
        if environment.client_cohort != expanded.effective_environment.client_cohort
            || environment.workspace_root_identity
                != expanded.effective_environment.workspace_root_identity
            || environment.image_digest != expanded.effective_environment.image_digest
            || environment.host.filesystem != expanded.effective_environment.filesystem
            || environment.host.free_space_bytes != expanded.effective_environment.free_space_bytes
        {
            return Err(SchedulerError::InvalidManifestAuthority(
                "captured environment does not match the expanded effective environment".to_owned(),
            ));
        }
        if expanded.effective_environment.gateway_mode != GatewayMode::Isolated {
            return Err(SchedulerError::InvalidManifestAuthority(
                "the v1 runner requires an isolated gateway".to_owned(),
            ));
        }
        validate_sha256_identity(&definition_snapshot_sha256)?;

        let operation_authorities =
            operation_authorities(expanded, definition_snapshot, environment.client_cohort)?;
        let revisions =
            definition_revision_identities(definition_snapshot, &operation_authorities)?;

        Ok(Self {
            schema_version: RUN_MANIFEST_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            name: expanded.canonical_plan.name.clone(),
            plan_hash: expanded.plan_hash.clone(),
            starting_preset,
            state: RunState::Planned,
            producer: ProducerIdentity {
                package: env!("CARGO_PKG_NAME").to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            artifact_schemas: artifact_schema_set(
                expanded.canonical_plan.schema_version,
                expanded.schema_version,
                definition_snapshot.schema_version,
                environment.schema_version,
            )?,
            definition_snapshot: DefinitionSnapshotIdentity {
                schema_name: DEFINITION_SNAPSHOT_SCHEMA_NAME.to_owned(),
                schema_version: definition_snapshot.schema_version,
                sha256: definition_snapshot_sha256,
            },
            operation_authorities,
            metric_revisions: revisions.metrics,
            check_revisions: revisions.checks,
            phase_revisions: revisions.phases,
            fixed_lifecycle_policy: expanded.fixed_lifecycle_policy,
            failure_policy: effective_failure_policy(expanded.fixed_lifecycle_policy),
            effective_timeouts: effective_timeout_policy()?,
            gateway_policy: effective_gateway_policy(expanded)?,
            safety_policy: effective_safety_policy(expanded)?,
            treatment: environment.treatment.clone(),
            environment,
            fixture_generator_revision: fixtures::FIXTURE_GENERATOR_VERSION,
            fixture_hashes: BTreeMap::new(),
            created_at: wall_timestamp()?,
            started_at: None,
            ended_at: None,
            failure: None,
        })
    }
}

fn schema_identity(
    schema_name: &str,
    write_version: u32,
    read_versions: Vec<u32>,
) -> Result<ArtifactSchemaIdentity, SchedulerError> {
    if schema_name.is_empty() || write_version == 0 || read_versions.is_empty() {
        return Err(SchedulerError::InvalidManifestAuthority(
            "artifact schema identities require a name and positive versions".to_owned(),
        ));
    }
    let mut canonical = read_versions.clone();
    canonical.sort_unstable();
    canonical.dedup();
    if canonical != read_versions || !read_versions.contains(&write_version) {
        return Err(SchedulerError::InvalidManifestAuthority(format!(
            "artifact schema {schema_name} has non-canonical read versions"
        )));
    }
    Ok(ArtifactSchemaIdentity {
        schema_name: schema_name.to_owned(),
        write_version,
        read_versions,
    })
}

fn artifact_schema_set(
    intent_plan_version: u32,
    expanded_plan_version: u32,
    definition_snapshot_version: u32,
    environment_metadata_version: u32,
) -> Result<ArtifactSchemaSet, SchedulerError> {
    if OBSERVATION_SCHEMA_VERSION != 3 {
        return Err(SchedulerError::InvalidManifestAuthority(format!(
            "observation write version {} is missing an explicit manifest decoder declaration",
            OBSERVATION_SCHEMA_VERSION
        )));
    }
    Ok(ArtifactSchemaSet {
        run_manifest: schema_identity(
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            vec![RUN_MANIFEST_SCHEMA_VERSION],
        )?,
        intent_plan: schema_identity(
            INTENT_PLAN_SCHEMA_NAME,
            intent_plan_version,
            vec![intent_plan_version],
        )?,
        expanded_plan: schema_identity(
            EXPANDED_PLAN_SCHEMA_NAME,
            expanded_plan_version,
            vec![expanded_plan_version],
        )?,
        definition_snapshot: schema_identity(
            DEFINITION_SNAPSHOT_SCHEMA_NAME,
            definition_snapshot_version,
            vec![definition_snapshot_version],
        )?,
        environment_metadata: schema_identity(
            ENVIRONMENT_METADATA_SCHEMA_NAME,
            environment_metadata_version,
            vec![environment_metadata_version],
        )?,
        events: schema_identity(
            EVENT_SCHEMA_NAME,
            EVENT_SCHEMA_VERSION,
            vec![EVENT_SCHEMA_VERSION],
        )?,
        observations: schema_identity(
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
            vec![1, 2, 3],
        )?,
        bounded_evidence: schema_identity(
            BOUNDED_EVIDENCE_SCHEMA_NAME,
            BOUNDED_EVIDENCE_SCHEMA_VERSION,
            vec![BOUNDED_EVIDENCE_SCHEMA_VERSION],
        )?,
    })
}

fn operation_authorities(
    expanded: &ExpandedPlan,
    definitions: &DefinitionCatalog,
    client_cohort: ClientCohort,
) -> Result<Vec<OperationAuthority>, SchedulerError> {
    let selected = expanded
        .cells
        .iter()
        .map(|cell| cell.operation_id)
        .collect::<BTreeSet<_>>();
    let mut authorities = Vec::with_capacity(selected.len());
    for operation_id in OperationId::ALL {
        if !selected.contains(&operation_id) {
            continue;
        }
        let definition = definitions
            .operations
            .iter()
            .find(|definition| definition.id == operation_id)
            .copied()
            .ok_or_else(|| {
                SchedulerError::InvalidManifestAuthority(format!(
                    "definition snapshot omits selected operation {operation_id:?}"
                ))
            })?;
        if !definition.supported_cohorts.contains(&client_cohort) {
            return Err(SchedulerError::InvalidManifestAuthority(format!(
                "operation {operation_id:?} does not support cohort {client_cohort:?}"
            )));
        }

        let cells = expanded
            .cells
            .iter()
            .filter(|cell| cell.operation_id == operation_id)
            .collect::<Vec<_>>();
        let mut isolation = Vec::new();
        let mut timeouts = BTreeSet::new();
        for cell in cells {
            if cell.family_id != definition.family
                || cell.operation.id() != operation_id
                || cell.operation_semantic_revision != definition.semantic_revision
                || cell.factor_schema_revision != definition.factor_schema_revision
                || cell.comparison_key.operation != operation_id
                || cell.comparison_key.semantic_revision != definition.semantic_revision
                || cell.comparison_key.factor_schema_revision != definition.factor_schema_revision
                || cell.comparison_key.comparison_projection_revision
                    != definition.comparison.semantic_revision
                || cell.comparison_key.product_access != definition.product_access
                || cell.comparison_key.count_semantics != definition.count_semantics
                || cell.comparison_key.isolation != cell.operation.resolved_isolation()
                || cell.protocol.cleanup != definition.cleanup
            {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "expanded cell {} disagrees with definition {operation_id:?}",
                    cell.cell_id
                )));
            }
            let resolved = cell.operation.resolved_isolation();
            if !isolation.contains(&resolved) {
                isolation.push(resolved);
            }
            timeouts.insert(cell.protocol.timeout_ms);
        }
        authorities.push(OperationAuthority {
            operation_id,
            family_id: definition.family,
            semantic_revision: definition.semantic_revision,
            factor_schema_revision: definition.factor_schema_revision,
            comparison_projection_revision: definition.comparison.semantic_revision,
            client_cohort,
            product_access: definition.product_access,
            count_semantics: definition.count_semantics,
            cleanup_policy: definition.cleanup,
            resolved_isolation_policies: isolation,
            request_timeout_ms: timeouts.into_iter().collect(),
            stabilization_policy: stabilization_policy(
                operation_id,
                expanded.fixed_lifecycle_policy.stabilization_revision,
            )?,
        });
    }
    Ok(authorities)
}

fn stabilization_policy(
    operation_id: OperationId,
    semantic_revision: u32,
) -> Result<StabilizationPolicy, SchedulerError> {
    match operation_id {
        OperationId::ExecCommand
        | OperationId::FileRead
        | OperationId::FileWrite
        | OperationId::FileEdit
        | OperationId::FileBlame
        | OperationId::CreateWorkspace => {
            Ok(StabilizationPolicy::NotRequired { semantic_revision })
        }
        OperationId::SquashLayerstack => Ok(StabilizationPolicy::ExactSnapshotQuietWindow {
            semantic_revision,
            quiet_window_matches: u32::try_from(LAYERSTACK_SETTLE_MATCHES).map_err(|_| {
                SchedulerError::InvalidManifestAuthority(
                    "layerstack settle match count does not fit u32".to_owned(),
                )
            })?,
            poll_interval_ms: fixed_duration_ms(LAYERSTACK_SETTLE_INTERVAL)?,
            timeout_ms: fixed_duration_ms(LAYERSTACK_SETTLE_TIMEOUT)?,
        }),
    }
}

fn definition_revision_identities(
    definitions: &DefinitionCatalog,
    operations: &[OperationAuthority],
) -> Result<ManifestRevisionIdentities, SchedulerError> {
    let mut metrics = BTreeMap::new();
    for metric in definitions.metrics {
        if let Some(previous) = metrics.insert(metric.id.to_owned(), metric.semantic_revision) {
            if previous != metric.semantic_revision {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "metric {} has conflicting revisions",
                    metric.id
                )));
            }
        }
    }

    let selected = operations
        .iter()
        .map(|authority| authority.operation_id)
        .collect::<BTreeSet<_>>();
    let mut checks = BTreeMap::new();
    let mut phases = BTreeMap::new();
    for definition in &definitions.operations {
        if !selected.contains(&definition.id) {
            continue;
        }
        for check in definition.checks {
            if checks
                .insert(check.id, check.semantic_revision)
                .is_some_and(|previous| previous != check.semantic_revision)
            {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "check {:?} has conflicting revisions",
                    check.id
                )));
            }
        }
        for phase in definition.phases {
            if phases
                .insert(phase.id, phase.semantic_revision)
                .is_some_and(|previous| previous != phase.semantic_revision)
            {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "phase {:?} has conflicting revisions",
                    phase.id
                )));
            }
        }
    }
    Ok(ManifestRevisionIdentities {
        metrics: metrics
            .into_iter()
            .map(|(metric_id, semantic_revision)| MetricRevisionIdentity {
                metric_id,
                semantic_revision,
            })
            .collect(),
        checks: checks
            .into_iter()
            .map(|(check_id, semantic_revision)| CheckRevisionIdentity {
                check_id,
                semantic_revision,
            })
            .collect(),
        phases: phases
            .into_iter()
            .map(|(phase_id, semantic_revision)| PhaseRevisionIdentity {
                phase_id,
                semantic_revision,
            })
            .collect(),
    })
}

fn effective_failure_policy(lifecycle: FixedLifecyclePolicy) -> EffectiveFailurePolicy {
    EffectiveFailurePolicy {
        semantic_revision: lifecycle.failure_revision,
        product_transport_timeout_or_correctness: FailureAction::ContinueWhenEnvironmentSafe,
        fixture_containment_ownership_environment_or_infrastructure: FailureAction::AbortCampaign,
        teardown_or_cleanup_baseline: FailureAction::AbortCampaign,
        automatic_measured_operation_retries: lifecycle.automatic_retries,
    }
}

fn effective_timeout_policy() -> Result<EffectiveTimeoutPolicy, SchedulerError> {
    Ok(EffectiveTimeoutPolicy {
        sandbox_create_timeout_ms: fixed_duration_ms(SANDBOX_CREATE_TIMEOUT)?,
        sandbox_destroy_timeout_ms: fixed_duration_ms(SANDBOX_DESTROY_TIMEOUT)?,
        operation_teardown_timeout_ms: fixed_duration_ms(OPERATION_TEARDOWN_TIMEOUT)?,
        gateway_stop_timeout_ms: fixed_duration_ms(GATEWAY_STOP_TIMEOUT)?,
        gateway_owned_resource_cleanup_timeout_ms: fixed_duration_ms(
            OWNED_RESOURCE_CLEANUP_TIMEOUT,
        )?,
        gateway_shutdown_timeout_ms: fixed_duration_ms(SHUTDOWN_TIMEOUT)?,
        gateway_log_drain_timeout_ms: fixed_duration_ms(LOG_DRAIN_TIMEOUT)?,
        layerstack_observation_timeout_ms: fixed_duration_ms(LAYERSTACK_OBSERVE_TIMEOUT)?,
        layerstack_trace_retry_timeout_ms: fixed_duration_ms(LAYERSTACK_TRACE_RETRY_TIMEOUT)?,
        product_resource_observation_timeout_ms: fixed_duration_ms(
            PRODUCT_RESOURCE_OBSERVE_TIMEOUT,
        )?,
    })
}

pub(crate) fn effective_gateway_policy(
    expanded: &ExpandedPlan,
) -> Result<EffectiveGatewayPolicy, SchedulerError> {
    let by_id = expanded
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell))
        .collect::<BTreeMap<_, _>>();
    let mut widths = BTreeSet::new();
    for block in &expanded.execution_blocks {
        let cells = block
            .cell_ids
            .iter()
            .map(|cell_id| {
                by_id.get(cell_id.as_str()).copied().ok_or_else(|| {
                    SchedulerError::InvalidManifestAuthority(format!(
                        "execution block {} references missing cell {cell_id}",
                        block.block_id
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        widths.insert(block_remount_parallelism(block.family_id, &cells)?);
    }
    Ok(EffectiveGatewayPolicy {
        semantic_revision: 1,
        mode: expanded.effective_environment.gateway_mode,
        loopback_only: true,
        isolated_runtime_per_execution_block: true,
        remount_sweep_widths: widths.into_iter().collect(),
        maximum_connections: fixed_usize(MAX_GATEWAY_CONNECTIONS, "gateway connection cap")?,
        readiness_timeout_ms: fixed_duration_ms(READINESS_TIMEOUT)?,
        readiness_probe_timeout_ms: fixed_duration_ms(READINESS_PROBE_TIMEOUT)?,
        readiness_poll_interval_ms: fixed_duration_ms(READINESS_POLL)?,
    })
}

pub(crate) fn effective_safety_policy(
    expanded: &ExpandedPlan,
) -> Result<EffectiveSafetyPolicy, SchedulerError> {
    let campaign = |requested, fixed_maximum| CapResolution {
        requested,
        effective: requested,
        fixed_maximum,
        unit: CapUnit::Count,
        cap_revision: 1,
    };
    Ok(EffectiveSafetyPolicy {
        semantic_revision: 1,
        cap_behavior: CapBehavior::RejectAboveMaximum,
        campaign_caps: CampaignCapResolutions {
            expanded_test_combinations: campaign(expanded.estimates.cell_count, MAX_EXPANDED_CELLS),
            trial_batches: campaign(expanded.estimates.trial_batch_count, MAX_TRIAL_BATCH_COUNT),
            issued_product_requests: campaign(
                expanded.estimates.issued_operation_request_count,
                MAX_ISSUED_OPERATION_REQUEST_COUNT,
            ),
        },
        product_caps: product_cap_resolutions(expanded)?,
        fixed_gateway_caps: FixedGatewaySafetyCaps {
            log_bytes: fixed_usize(MAX_LOG_BYTES, "gateway log cap")?,
            log_line_bytes: fixed_usize(MAX_LOG_LINE_BYTES, "gateway log line cap")?,
            product_path_bytes: fixed_usize(MAX_PRODUCT_PATH_BYTES, "product path cap")?,
            product_content_bytes: fixed_usize(MAX_PRODUCT_CONTENT_BYTES, "product content cap")?,
            product_edits: fixed_usize(MAX_PRODUCT_EDITS, "product edit cap")?,
            command_timeout_ms: MAX_COMMAND_TIMEOUT_MS,
            product_trace_nodes: fixed_usize(MAX_PRODUCT_TRACE_NODES, "trace node cap")?,
            product_resource_window_ms: PRODUCT_RESOURCE_WINDOW_MS,
            product_resource_samples: fixed_usize(
                MAX_PRODUCT_RESOURCE_SAMPLES,
                "resource sample cap",
            )?,
        },
    })
}

fn product_cap_resolutions(
    expanded: &ExpandedPlan,
) -> Result<Vec<ProductCapResolution>, SchedulerError> {
    let mut caps = BTreeMap::<(OperationId, ProductCapId), ProductCapResolution>::new();
    for expanded_cell in &expanded.cells {
        let mut record = |cap_id, requested, fixed_maximum, unit| {
            if requested > fixed_maximum {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "operation {:?} requests {requested} above fixed cap {fixed_maximum}",
                    expanded_cell.operation_id
                )));
            }
            let entry =
                caps.entry((expanded_cell.operation_id, cap_id))
                    .or_insert(ProductCapResolution {
                        operation_id: expanded_cell.operation_id,
                        cap_id,
                        maximum_requested: requested,
                        maximum_effective: requested,
                        fixed_maximum,
                        unit,
                        cap_revision: 1,
                    });
            if entry.fixed_maximum != fixed_maximum || entry.unit != unit {
                return Err(SchedulerError::InvalidManifestAuthority(format!(
                    "operation {:?} cap {cap_id:?} has inconsistent definitions",
                    expanded_cell.operation_id
                )));
            }
            entry.maximum_requested = entry.maximum_requested.max(requested);
            entry.maximum_effective = entry.maximum_effective.max(requested);
            Ok(())
        };
        match &expanded_cell.operation {
            ExpandedOperationCell::ExecCommand(cell) => record(
                ProductCapId::StoredCommandOutput,
                cell.output_limit_bytes,
                executors::command::STORED_OUTPUT_LIMIT_BYTES,
                CapUnit::Bytes,
            )?,
            ExpandedOperationCell::FileRead(cell) => record(
                ProductCapId::FileReadReturnedBytes,
                cell.returned_bytes,
                executors::files::MAX_RUNTIME_READ_BYTES,
                CapUnit::Bytes,
            )?,
            ExpandedOperationCell::FileWrite(cell) => record(
                ProductCapId::FileWriteContentBytes,
                cell.content_bytes,
                executors::files::MAX_RUNTIME_CONTENT_BYTES,
                CapUnit::Bytes,
            )?,
            ExpandedOperationCell::FileEdit(cell) => {
                record(
                    ProductCapId::FileEditFileBytes,
                    cell.file_bytes,
                    executors::files::MAX_RUNTIME_CONTENT_BYTES,
                    CapUnit::Bytes,
                )?;
                record(
                    ProductCapId::FileEditReplacementCount,
                    u64::from(cell.replacement_count),
                    u64::from(executors::files::MAX_RUNTIME_EDITS),
                    CapUnit::Count,
                )?;
            }
            ExpandedOperationCell::FileBlame(cell) => record(
                ProductCapId::FileBlameAuditabilityEvents,
                u64::from(cell.auditability_event_count),
                u64::from(executors::files::MAX_AUDITABILITY_EVENTS),
                CapUnit::Count,
            )?,
            ExpandedOperationCell::CreateWorkspace(_) => {}
            ExpandedOperationCell::SquashLayerstack(cell) => {
                let prepared_layers = cell
                    .squashable_blocks
                    .checked_mul(cell.layers_per_block)
                    .ok_or_else(|| {
                        SchedulerError::InvalidManifestAuthority(
                            "layerstack prepared-layer count overflowed".to_owned(),
                        )
                    })?;
                record(
                    ProductCapId::LayerstackPreparedLayers,
                    u64::from(prepared_layers),
                    u64::from(executors::layerstack::MAX_PREPARED_LAYERS),
                    CapUnit::Count,
                )?;
                record(
                    ProductCapId::LayerstackPayloadBytes,
                    cell.payload_bytes,
                    executors::layerstack::MAX_LAYER_PAYLOAD_BYTES,
                    CapUnit::Bytes,
                )?;
            }
        }
    }
    Ok(caps.into_values().collect())
}

fn fixed_duration_ms(duration: Duration) -> Result<u64, SchedulerError> {
    u64::try_from(duration.as_millis()).map_err(|_| {
        SchedulerError::InvalidManifestAuthority("fixed timeout does not fit u64 ms".to_owned())
    })
}

fn fixed_usize(value: usize, label: &str) -> Result<u64, SchedulerError> {
    u64::try_from(value)
        .map_err(|_| SchedulerError::InvalidManifestAuthority(format!("{label} does not fit u64")))
}

fn validate_sha256_identity(value: &str) -> Result<(), SchedulerError> {
    let digest = value.strip_prefix("sha256:").ok_or_else(|| {
        SchedulerError::InvalidManifestAuthority(
            "definition snapshot hash must use the sha256: prefix".to_owned(),
        )
    })?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SchedulerError::InvalidManifestAuthority(
            "definition snapshot hash must contain 64 hexadecimal digits".to_owned(),
        ));
    }
    Ok(())
}

impl RunArtifacts {
    pub async fn create(
        store: ArtifactStore,
        run_id: &str,
        expanded: &ExpandedPlan,
        starting_preset: Option<PresetRef>,
        environment: EnvironmentMetadata,
        definition_snapshot: DefinitionCatalog,
    ) -> Result<Arc<Self>, SchedulerError> {
        store.create_run(run_id)?;
        let result = async {
            store.write_immutable(
                run_id,
                ArtifactId::IntentPlan,
                INTENT_PLAN_SCHEMA_NAME,
                expanded.canonical_plan.schema_version,
                &expanded.canonical_plan,
            )?;
            store.write_immutable(
                run_id,
                ArtifactId::ExpandedPlan,
                EXPANDED_PLAN_SCHEMA_NAME,
                expanded.schema_version,
                expanded,
            )?;
            store.write_immutable(
                run_id,
                ArtifactId::DefinitionSnapshot,
                DEFINITION_SNAPSHOT_SCHEMA_NAME,
                definition_snapshot.schema_version,
                &definition_snapshot,
            )?;
            store.write_immutable(
                run_id,
                ArtifactId::EnvironmentMetadata,
                ENVIRONMENT_METADATA_SCHEMA_NAME,
                environment.schema_version,
                &environment,
            )?;
            let definition_snapshot_sha256 = format!(
                "sha256:{:x}",
                Sha256::digest(
                    &store
                        .content(run_id, ArtifactId::DefinitionSnapshot.as_str())?
                        .bytes
                )
            );
            let manifest = RunManifest::planned(
                run_id,
                expanded,
                starting_preset,
                environment,
                &definition_snapshot,
                definition_snapshot_sha256,
            )?;
            store.replace_snapshot(
                run_id,
                ArtifactId::RunManifest,
                RUN_MANIFEST_SCHEMA_NAME,
                RUN_MANIFEST_SCHEMA_VERSION,
                &manifest,
            )?;
            let events = EventJournal::open(store.clone(), run_id).await?;
            Ok(Arc::new(Self {
                run_id: run_id.to_owned(),
                events,
                store: store.clone(),
                manifest: Mutex::new(manifest),
                observation_sequence: tokio::sync::Mutex::new(0),
            }))
        }
        .await;
        if result.is_err() {
            let _ = store.remove_incomplete_run(run_id);
        }
        result
    }

    pub fn manifest(&self) -> Result<RunManifest, SchedulerError> {
        self.manifest
            .lock()
            .map(|manifest| manifest.clone())
            .map_err(|_| SchedulerError::ManifestPoisoned)
    }

    #[must_use]
    pub fn store(&self) -> ArtifactStore {
        self.store.clone()
    }

    pub fn record_fixture_hash(
        &self,
        fixture_id: impl Into<String>,
        fixture_hash: impl Into<String>,
    ) -> Result<(), SchedulerError> {
        let fixture_id = fixture_id.into();
        let fixture_hash = fixture_hash.into();
        let mut manifest = self
            .manifest
            .lock()
            .map_err(|_| SchedulerError::ManifestPoisoned)?;
        if is_terminal(manifest.state) {
            return Err(SchedulerError::TerminalManifest);
        }
        if let Some(existing) = manifest.fixture_hashes.get(&fixture_id) {
            if existing == &fixture_hash {
                return Ok(());
            }
            return Err(SchedulerError::ArtifactTask(format!(
                "fixture identity {fixture_id} changed within one run"
            )));
        }
        manifest.fixture_hashes.insert(fixture_id, fixture_hash);
        self.store.replace_snapshot(
            &self.run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &*manifest,
        )?;
        Ok(())
    }

    pub fn transition(
        &self,
        state: RunState,
        failure: Option<RunFailure>,
    ) -> Result<RunManifest, SchedulerError> {
        let mut manifest = self
            .manifest
            .lock()
            .map_err(|_| SchedulerError::ManifestPoisoned)?;
        if is_terminal(manifest.state) {
            return Err(SchedulerError::TerminalManifest);
        }
        if !valid_manifest_transition(manifest.state, state) {
            return Err(SchedulerError::InvalidManifestTransition {
                from: manifest.state,
                to: state,
            });
        }
        let mut next = manifest.clone();
        if state == RunState::Running && next.started_at.is_none() {
            next.started_at = Some(wall_timestamp()?);
        }
        next.state = state;
        next.failure = failure;
        if is_terminal(state) {
            next.ended_at = Some(wall_timestamp()?);
        }
        self.store.replace_snapshot(
            &self.run_id,
            ArtifactId::RunManifest,
            RUN_MANIFEST_SCHEMA_NAME,
            RUN_MANIFEST_SCHEMA_VERSION,
            &next,
        )?;
        *manifest = next.clone();
        Ok(next)
    }

    /// Durably records a cancellation request and only then signals the owned
    /// campaign token. Both actions happen while the manifest transition lock
    /// is held, so scheduler transitions cannot enter the persistence/signal
    /// window and future trial scheduling observes the signal before unlock.
    pub fn request_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(RunManifest, bool), SchedulerError> {
        let mut manifest = self
            .manifest
            .lock()
            .map_err(|_| SchedulerError::ManifestPoisoned)?;
        if is_terminal(manifest.state) {
            return Ok((manifest.clone(), false));
        }
        let newly_requested = !cancellation.is_cancelled();
        if manifest.state != RunState::Cancelling {
            if !valid_manifest_transition(manifest.state, RunState::Cancelling) {
                return Err(SchedulerError::InvalidManifestTransition {
                    from: manifest.state,
                    to: RunState::Cancelling,
                });
            }
            let mut next = manifest.clone();
            next.state = RunState::Cancelling;
            next.failure = None;
            self.store.replace_snapshot(
                &self.run_id,
                ArtifactId::RunManifest,
                RUN_MANIFEST_SCHEMA_NAME,
                RUN_MANIFEST_SCHEMA_VERSION,
                &next,
            )?;
            *manifest = next;
        }
        cancellation.cancel();
        Ok((manifest.clone(), newly_requested))
    }

    pub async fn append_observation(
        &self,
        observation: ObservationRecord,
    ) -> Result<(), SchedulerError> {
        let mut sequence = self.observation_sequence.lock().await;
        *sequence = sequence.saturating_add(1);
        let observation = SequencedObservation {
            sequence: *sequence,
            record: observation,
        };
        let store = self.store.clone();
        let run_id = self.run_id.clone();
        tokio::task::spawn_blocking(move || {
            store.append_record(
                &run_id,
                ArtifactId::Observations,
                OBSERVATION_SCHEMA_NAME,
                OBSERVATION_SCHEMA_VERSION,
                &observation,
            )
        })
        .await
        .map_err(|error| SchedulerError::ArtifactTask(error.to_string()))??;
        Ok(())
    }

    pub async fn write_trial_evidence(
        &self,
        cell_id: &str,
        trial_id: &str,
        evidence: OperationEvidence,
    ) -> Result<ArtifactRef, SchedulerError> {
        let store = self.store.clone();
        let run_id = self.run_id.clone();
        let cell_id = cell_id.to_owned();
        let trial_id = trial_id.to_owned();
        tokio::task::spawn_blocking(move || {
            store.write_trial_evidence(&run_id, &cell_id, &trial_id, &evidence)
        })
        .await
        .map_err(|error| SchedulerError::ArtifactTask(error.to_string()))?
        .map_err(SchedulerError::from)
    }
}

#[must_use]
pub const fn is_terminal(state: RunState) -> bool {
    match state {
        RunState::Planned
        | RunState::Queued
        | RunState::Preparing
        | RunState::Running
        | RunState::Verifying
        | RunState::TearingDown
        | RunState::Cancelling => false,
        RunState::Completed | RunState::Failed | RunState::Cancelled => true,
    }
}

const fn valid_manifest_transition(from: RunState, to: RunState) -> bool {
    match from {
        RunState::Planned => matches!(
            to,
            RunState::Queued | RunState::Cancelling | RunState::Failed
        ),
        RunState::Queued => matches!(
            to,
            RunState::Preparing | RunState::Cancelling | RunState::Failed
        ),
        RunState::Preparing => matches!(
            to,
            RunState::Running | RunState::Cancelling | RunState::Failed
        ),
        RunState::Running => matches!(
            to,
            RunState::Verifying | RunState::Cancelling | RunState::Failed
        ),
        RunState::Verifying => matches!(
            to,
            RunState::TearingDown | RunState::Cancelling | RunState::Failed
        ),
        RunState::TearingDown => matches!(
            to,
            RunState::Completed | RunState::Cancelling | RunState::Failed
        ),
        RunState::Cancelling => matches!(to, RunState::Cancelled),
        RunState::Completed | RunState::Failed | RunState::Cancelled => false,
    }
}

pub fn wall_timestamp() -> Result<String, time::error::Format> {
    OffsetDateTime::now_utc().format(&Rfc3339)
}

#[derive(Debug)]
struct MaterializedProfiles {
    by_id: BTreeMap<WorkspaceProfileId, MaterializedFixture>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CampaignControl {
    Continue,
    Cancelled,
}

/// Executes one validated expanded campaign through the closed local product
/// access path. Submission/idempotency and the single-campaign gate remain API
/// concerns; this entry point owns the complete durable execution lifecycle.
pub async fn run_campaign(
    startup: StartupConfig,
    dependencies: ExecutionDependencies,
    expanded: ExpandedPlan,
    artifacts: Arc<RunArtifacts>,
    cancellation: CancellationToken,
) -> Result<RunManifest, SchedulerError> {
    let clock = Arc::new(MonotonicClock::start());
    match run_campaign_lifecycle(
        &startup,
        &dependencies,
        &expanded,
        &artifacts,
        &cancellation,
        &clock,
    )
    .await
    {
        Ok(manifest) => Ok(manifest),
        Err(error) => {
            let code = scheduler_failure_code(&error);
            let message = error.to_string();
            terminalize_campaign_failure(
                &artifacts,
                Some(&expanded),
                if cancellation.is_cancelled() {
                    RunState::Cancelled
                } else {
                    RunState::Failed
                },
                code,
                &message,
                clock.offset_ns(),
            )
            .await
        }
    }
}

/// Persists a failed terminal campaign when the owner task could not enter or
/// return from [`run_campaign`] (for example, a task spawn or join failure).
/// The helper deliberately needs only the already-created artifact handle so
/// the API can call it before releasing the single-campaign gate.
pub async fn fail_campaign(
    artifacts: Arc<RunArtifacts>,
    code: &str,
    message: &str,
) -> Result<RunManifest, SchedulerError> {
    let expanded = artifacts
        .store()
        .read_envelope::<ExpandedPlan>(
            &artifacts.run_id,
            ArtifactId::ExpandedPlan,
            EXPANDED_PLAN_SCHEMA_NAME,
            crate::plan::EXPANDED_PLAN_SCHEMA_VERSION,
        )
        .ok();
    terminalize_campaign_failure(
        &artifacts,
        expanded.as_ref(),
        RunState::Failed,
        code,
        message,
        EMERGENCY_EVENT_OFFSET_NS,
    )
    .await
}

async fn run_campaign_lifecycle(
    startup: &StartupConfig,
    dependencies: &ExecutionDependencies,
    expanded: &ExpandedPlan,
    artifacts: &Arc<RunArtifacts>,
    cancellation: &CancellationToken,
    clock: &Arc<MonotonicClock>,
) -> Result<RunManifest, SchedulerError> {
    validate_campaign_shape(expanded)?;

    let entered_preparing =
        advance_campaign_state(artifacts, clock, cancellation, RunState::Preparing).await?;

    let execution = if entered_preparing == CampaignControl::Cancelled {
        Ok(CampaignControl::Cancelled)
    } else if expanded.canonical_plan.environment.client_cohort != ClientCohort::DirectClient {
        Err(SchedulerError::UnsupportedClientCohort(
            expanded.canonical_plan.environment.client_cohort,
        ))
    } else {
        match materialize_profiles(startup, expanded, artifacts).await {
            Ok(profiles) => {
                if advance_campaign_state(artifacts, clock, cancellation, RunState::Running).await?
                    == CampaignControl::Cancelled
                {
                    Ok(CampaignControl::Cancelled)
                } else {
                    execute_campaign(
                        startup,
                        dependencies,
                        expanded,
                        artifacts,
                        &profiles,
                        clock,
                        cancellation,
                    )
                    .await
                }
            }
            Err(error) => Err(error),
        }
    };

    // Run states describe aggregate campaign work only. Every trial has
    // already emitted its independent setup/operation/verify/teardown phases
    // while the aggregate run was `running`.
    let execution = match execution {
        Ok(CampaignControl::Continue) => {
            verify_and_enter_campaign_teardown(artifacts, cancellation, clock).await
        }
        other => other,
    };

    let (terminal_state, failure) = match execution {
        Ok(CampaignControl::Continue) => (RunState::Completed, None),
        Ok(CampaignControl::Cancelled) => {
            transition_to_cancelling(artifacts, clock).await?;
            (RunState::Cancelled, None)
        }
        Err(error) => {
            emit_warning(artifacts, clock, "campaign_aborted", &error.to_string()).await?;
            ensure_failure_observation(expanded, artifacts).await?;
            (
                if cancellation.is_cancelled() {
                    transition_to_cancelling(artifacts, clock).await?;
                    RunState::Cancelled
                } else {
                    RunState::Failed
                },
                Some(RunFailure {
                    code: scheduler_failure_code(&error).to_owned(),
                    message: bounded_event_text(&error.to_string()),
                    infrastructure: true,
                }),
            )
        }
    };

    // Successful campaigns were authoritatively verified before entering
    // `tearing_down`. For failed/cancelled campaigns, retain a best-effort
    // provisional projection without hiding the primary terminal outcome.
    if terminal_state != RunState::Completed {
        if let Err(error) = report::regenerate(&artifacts.store(), &artifacts.run_id, true) {
            emit_warning(
                artifacts,
                clock,
                "provisional_report_failed",
                &error.to_string(),
            )
            .await?;
        }
    }

    let (terminal_state, failure) =
        if terminal_state == RunState::Completed && cancellation.is_cancelled() {
            transition_to_cancelling(artifacts, clock).await?;
            (RunState::Cancelled, None)
        } else {
            (terminal_state, failure)
        };
    let manifest = match artifacts.transition(terminal_state, failure) {
        Ok(manifest) => manifest,
        Err(error)
            if terminal_state == RunState::Completed
                && cancellation_preempted_transition(&error, RunState::Completed) =>
        {
            transition_to_cancelling(artifacts, clock).await?;
            artifacts.transition(RunState::Cancelled, None)?
        }
        Err(error) => return Err(error),
    };
    emit_run_state(artifacts, clock, manifest.state).await?;
    report::regenerate(&artifacts.store(), &artifacts.run_id, false)?;
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::ReportReady { provisional: false },
        )
        .await?;
    Ok(manifest)
}

async fn advance_campaign_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cancellation: &CancellationToken,
    target: RunState,
) -> Result<CampaignControl, SchedulerError> {
    if cancellation.is_cancelled() {
        return Ok(CampaignControl::Cancelled);
    }
    match artifacts.transition(target, None) {
        Ok(_) => {
            emit_run_state(artifacts, clock, target).await?;
            Ok(CampaignControl::Continue)
        }
        Err(error) if cancellation_preempted_transition(&error, target) => {
            Ok(CampaignControl::Cancelled)
        }
        Err(error) => Err(error),
    }
}

async fn verify_and_enter_campaign_teardown(
    artifacts: &RunArtifacts,
    cancellation: &CancellationToken,
    clock: &MonotonicClock,
) -> Result<CampaignControl, SchedulerError> {
    if advance_campaign_state(artifacts, clock, cancellation, RunState::Verifying).await?
        == CampaignControl::Cancelled
    {
        return Ok(CampaignControl::Cancelled);
    }

    // Regeneration is the closed aggregate verification pass: it validates
    // durable observations, evidence links, definitions, checks, and the
    // statistics projection without loading an executor.
    report::regenerate(&artifacts.store(), &artifacts.run_id, true)?;

    // All product/gateway/workspace cleanup completed inside each execution
    // block before verification. Persisting this boundary records that the
    // campaign is now finalizing artifacts and the terminal report only; it
    // must not be interpreted as a trial teardown phase.
    let control =
        advance_campaign_state(artifacts, clock, cancellation, RunState::TearingDown).await?;
    if control == CampaignControl::Continue {
        artifacts
            .events
            .emit(
                clock.offset_ns(),
                EventData::ReportReady { provisional: true },
            )
            .await?;
    }
    Ok(control)
}

async fn terminalize_campaign_failure(
    artifacts: &RunArtifacts,
    expanded: Option<&ExpandedPlan>,
    terminal_state: RunState,
    code: &str,
    message: &str,
    event_offset_ns: u64,
) -> Result<RunManifest, SchedulerError> {
    let current = artifacts.manifest()?;
    if is_terminal(current.state) {
        return persist_terminal_outputs(artifacts, current, event_offset_ns).await;
    }

    // These diagnostics are best-effort. A damaged event or observation tail
    // must not prevent the manifest transition and report regeneration from
    // being attempted.
    let _ = artifacts
        .events
        .emit(
            event_offset_ns,
            EventData::Warning {
                code: "campaign_aborted".to_owned(),
                message: bounded_event_text(message),
            },
        )
        .await;
    if let Some(expanded) = expanded {
        let _ = ensure_failure_observation(expanded, artifacts).await;
    }
    let _ = report::regenerate(&artifacts.store(), &artifacts.run_id, true);

    // Once cancellation is durable, cleanup/report failures remain evidence on
    // a cancelled run rather than illegally changing its terminal class.
    let terminal_state = if artifacts.manifest()?.state == RunState::Cancelling {
        RunState::Cancelled
    } else {
        terminal_state
    };
    if terminal_state == RunState::Cancelled && artifacts.manifest()?.state != RunState::Cancelling
    {
        artifacts.transition(RunState::Cancelling, None)?;
        let _ = artifacts
            .events
            .emit(
                event_offset_ns,
                EventData::RunState {
                    state: RunState::Cancelling,
                },
            )
            .await;
    }

    let failure = RunFailure {
        code: bounded_failure_code(code),
        message: bounded_event_text(message),
        infrastructure: true,
    };
    let manifest = match artifacts.transition(terminal_state, Some(failure)) {
        Ok(manifest) => manifest,
        Err(SchedulerError::TerminalManifest) => artifacts.manifest()?,
        Err(error) => return Err(error),
    };
    persist_terminal_outputs(artifacts, manifest, event_offset_ns).await
}

async fn persist_terminal_outputs(
    artifacts: &RunArtifacts,
    manifest: RunManifest,
    event_offset_ns: u64,
) -> Result<RunManifest, SchedulerError> {
    if !is_terminal(manifest.state) {
        return Err(SchedulerError::CampaignTask(
            "terminal output finalization requires a terminal manifest".to_owned(),
        ));
    }

    // Attempt report derivation even if terminal event persistence fails. This
    // gives the API a precise error while maximizing the durable evidence left
    // behind before it releases the campaign gate.
    let run_state_result = artifacts
        .events
        .emit(
            event_offset_ns,
            EventData::RunState {
                state: manifest.state,
            },
        )
        .await
        .map_err(SchedulerError::from);
    let report_result = report::regenerate(&artifacts.store(), &artifacts.run_id, false)
        .map_err(SchedulerError::from);
    let report_ready_result = if report_result.is_ok() {
        artifacts
            .events
            .emit(
                event_offset_ns,
                EventData::ReportReady { provisional: false },
            )
            .await
            .map(|_| ())
            .map_err(SchedulerError::from)
    } else {
        Ok(())
    };

    run_state_result?;
    report_result?;
    report_ready_result?;
    Ok(manifest)
}

fn validate_campaign_shape(expanded: &ExpandedPlan) -> Result<(), SchedulerError> {
    if !expanded.runnable {
        return Err(SchedulerError::InvalidPlan(
            "expanded plan is marked non-runnable".to_owned(),
        ));
    }
    let cells = expanded
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell))
        .collect::<BTreeMap<_, _>>();
    if cells.len() != expanded.cells.len() {
        return Err(SchedulerError::InvalidPlan(
            "expanded cell identifiers are not unique".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut previous_family = None;
    for block in &expanded.execution_blocks {
        if block.cell_ids.is_empty() {
            return Err(SchedulerError::InvalidExecutionBlock(format!(
                "{} contains no cells",
                block.block_id
            )));
        }
        if previous_family.is_some_and(|family| family > block.family_id) {
            return Err(SchedulerError::InvalidExecutionBlock(
                "families are not in fixed order".to_owned(),
            ));
        }
        previous_family = Some(block.family_id);
        for cell_id in &block.cell_ids {
            let cell = cells.get(cell_id.as_str()).ok_or_else(|| {
                SchedulerError::InvalidExecutionBlock(format!(
                    "{} references unknown cell {cell_id}",
                    block.block_id
                ))
            })?;
            if cell.family_id != block.family_id
                || expected_family(cell.operation_id) != block.family_id
                || cell.operation.id() != cell.operation_id
            {
                return Err(SchedulerError::InvalidExecutionBlock(format!(
                    "cell {cell_id} does not match its typed family/operation"
                )));
            }
            if !seen.insert(cell_id.as_str()) {
                return Err(SchedulerError::InvalidExecutionBlock(format!(
                    "cell {cell_id} is scheduled more than once"
                )));
            }
        }
    }
    if seen.len() != expanded.cells.len() {
        return Err(SchedulerError::InvalidExecutionBlock(
            "one or more expanded cells are not scheduled".to_owned(),
        ));
    }
    Ok(())
}

async fn materialize_profiles(
    startup: &StartupConfig,
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
) -> Result<MaterializedProfiles, SchedulerError> {
    let root = startup.paths.fixtures.clone();
    let seed = expanded.canonical_plan.seed;
    let profiles = expanded.selected_workspace_profiles.clone();
    let fixtures = tokio::task::spawn_blocking(move || {
        profiles
            .into_iter()
            .map(|profile| {
                fixtures::materialize(&root, &profile, seed).map(|fixture| (profile.id, fixture))
            })
            .collect::<Result<Vec<_>, FixtureError>>()
    })
    .await
    .map_err(|error| SchedulerError::CampaignTask(error.to_string()))??;
    let mut by_id = BTreeMap::new();
    for (id, fixture) in fixtures {
        artifacts.record_fixture_hash(id.to_string(), fixture.manifest.fixture_hash.clone())?;
        by_id.insert(id, fixture);
    }
    Ok(MaterializedProfiles { by_id })
}

#[derive(Debug)]
struct BlockWorkspaceLedger {
    ledger: CleanupLedger,
    outstanding: BTreeMap<PathBuf, OwnedIdentity>,
}

impl BlockWorkspaceLedger {
    fn new() -> Self {
        Self {
            ledger: CleanupLedger::default(),
            outstanding: BTreeMap::new(),
        }
    }

    fn create_trial_root(
        &mut self,
        startup: &StartupConfig,
        run_id: &str,
        cell_id: &str,
        trial_id: &str,
    ) -> Result<(PathBuf, OwnedIdentity), SchedulerError> {
        let directory_name = trial_directory_name(run_id, cell_id, trial_id);
        let requested = startup.paths.runs.join(directory_name);
        fs::create_dir(&requested).map_err(|source| SchedulerError::CampaignIo {
            path: requested.clone(),
            source,
        })?;
        set_owner_only_directory(&requested)?;
        let identity = OwnedIdentity::RunTrial {
            run_id: run_id.to_owned(),
            trial_id: trial_id.to_owned(),
        };
        let canonical = match self
            .ledger
            .register(&startup.paths, &requested, identity.clone())
        {
            Ok(path) => path,
            Err(error) => {
                // The directory has not acquired benchmark cleanup authority;
                // it is still safe to remove because this function created it
                // exclusively and has not exposed it to the product.
                let _ = fs::remove_dir(&requested);
                return Err(error.into());
            }
        };
        self.outstanding.insert(canonical.clone(), identity.clone());
        Ok((canonical, identity))
    }

    fn remove_trial_root(
        &mut self,
        startup: &StartupConfig,
        path: &Path,
        identity: &OwnedIdentity,
    ) -> Result<(), SchedulerError> {
        self.ledger.remove_owned(&startup.paths, path, identity)?;
        self.outstanding.remove(path);
        Ok(())
    }

    fn remove_all_after_gateway_stop(
        &mut self,
        startup: &StartupConfig,
    ) -> Result<(), SchedulerError> {
        let outstanding = self
            .outstanding
            .iter()
            .map(|(path, identity)| (path.clone(), identity.clone()))
            .collect::<Vec<_>>();
        for (path, identity) in outstanding {
            self.remove_trial_root(startup, &path, &identity)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PreparedCellSandbox {
    workspace_root: PathBuf,
    workspace_identity: OwnedIdentity,
    sandbox_id: OwnedSandboxId,
}

#[derive(Debug)]
enum CellSandboxPreparation {
    Ready(PreparedCellSandbox),
    Cancelled,
}

const fn requires_prepared_cell_sandbox(policy: ResolvedIsolationPolicy) -> bool {
    match policy {
        ResolvedIsolationPolicy::ReusableVerifiedFixture
        | ResolvedIsolationPolicy::FreshSessionsPerTrial
        | ResolvedIsolationPolicy::PreparedSandboxPerCell => true,
        ResolvedIsolationPolicy::FreshSandboxPerTrial
        | ResolvedIsolationPolicy::FreshTopologyPerTrial => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_cell_sandbox(
    startup: &StartupConfig,
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
    profiles: &MaterializedProfiles,
    cell: &ExpandedCell,
    clock: &MonotonicClock,
    cancellation: &CancellationToken,
    product: &ProductGateway,
    workspaces: &mut BlockWorkspaceLedger,
) -> Result<CellSandboxPreparation, SchedulerError> {
    let (workspace_root, workspace_identity) = workspaces.create_trial_root(
        startup,
        &artifacts.run_id,
        &cell.cell_id,
        PREPARED_CELL_LIFECYCLE_ID,
    )?;

    if let Some(profile_id) = selected_profile_id(&cell.operation) {
        let fixture = profiles.by_id.get(profile_id).ok_or_else(|| {
            SchedulerError::InvalidPlan(format!(
                "cell {} selected unmaterialized workspace profile {profile_id}",
                cell.cell_id
            ))
        });
        let fixture = match fixture {
            Ok(fixture) => fixture,
            Err(error) => {
                workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
                return Err(error);
            }
        };
        let source = fixture.path.clone();
        let destination = workspace_root.clone();
        let copied = tokio::task::spawn_blocking(move || copy_fixture_tree(&source, &destination))
            .await
            .map_err(|error| SchedulerError::CampaignTask(error.to_string()))
            .and_then(std::convert::identity);
        if let Err(error) = copied {
            workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
            return Err(error);
        }
    }

    if cancellation.is_cancelled() {
        workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
        return Ok(CellSandboxPreparation::Cancelled);
    }

    let correlation = Correlation::new(
        &artifacts.run_id,
        &cell.cell_id,
        PREPARED_CELL_LIFECYCLE_ID,
        "create-sandbox",
    )?;
    let image = expanded.canonical_plan.environment.image.0.clone();
    let created = await_owned_task(
        cancellation,
        Some(SANDBOX_CREATE_TIMEOUT),
        CancellationBoundary::SandboxCreate,
        product.create_sandbox(&image, &workspace_root, &correlation),
    )
    .await;
    let sandbox_id = match created {
        OwnedTaskOutcome::Completed(Ok(sandbox_id)) => sandbox_id,
        OwnedTaskOutcome::Completed(Err(error)) => {
            // Transport failure is ambiguous: isolated gateway shutdown owns
            // product cleanup, and the root remains ledger-owned until then.
            return Err(SchedulerError::Gateway(error));
        }
        OwnedTaskOutcome::TimedOut => {
            return Err(SchedulerError::SandboxCreateTimeout(format!(
                "{}:{PREPARED_CELL_LIFECYCLE_ID}",
                cell.cell_id
            )));
        }
        OwnedTaskOutcome::CancelledBeforeStart => {
            workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
            return Ok(CellSandboxPreparation::Cancelled);
        }
        OwnedTaskOutcome::CancelledCompleted(Ok(sandbox_id)) => {
            let cleanup = destroy_trial_sandbox(
                startup,
                artifacts,
                cell,
                PREPARED_CELL_LIFECYCLE_ID,
                product,
                &sandbox_id,
                &workspace_root,
                &workspace_identity,
                workspaces,
            )
            .await;
            cleanup?;
            return Ok(CellSandboxPreparation::Cancelled);
        }
        OwnedTaskOutcome::CancelledCompleted(Err(error)) => {
            // The create response is ambiguous after cancellation. Retain the
            // marker-owned root until the isolated gateway has stopped.
            emit_warning(
                artifacts,
                clock,
                "cancelled_cell_create_ambiguous",
                &error.to_string(),
            )
            .await?;
            return Ok(CellSandboxPreparation::Cancelled);
        }
        OwnedTaskOutcome::CancelledAfterGrace => {
            emit_warning(
                artifacts,
                clock,
                "cell_create_cancellation_grace_expired",
                "sandbox creation did not settle within its cancellation grace; gateway shutdown owns cleanup",
            )
            .await?;
            return Ok(CellSandboxPreparation::Cancelled);
        }
    };
    Ok(CellSandboxPreparation::Ready(PreparedCellSandbox {
        workspace_root,
        workspace_identity,
        sandbox_id,
    }))
}

/// Compile-time lifecycle seam used by both the closed production dispatcher
/// and typed acceptance adapters. It cannot be stored as a trait object and it
/// does not expose HTTP, artifact, report, or statistics dependencies to an
/// operation implementation.
#[allow(async_fn_in_trait)]
pub trait StaticLifecycleDispatch {
    type Context: Sync;
    type Cell: Sync;
    type Prepared;
    type Invocation: RuntimeInvocation;
    type Outcome;

    async fn prepare(
        context: &Self::Context,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError>;

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError>;

    async fn invoke_one(context: &Self::Context, invocation: Self::Invocation) -> Self::Outcome;

    async fn verify(
        context: &Self::Context,
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[Self::Outcome],
    ) -> Result<Verification, ExecutorError>;

    async fn teardown(
        context: &Self::Context,
        prepared: &mut Self::Prepared,
    ) -> crate::executors::TeardownResult;

    fn outcome_succeeded(outcome: &Self::Outcome) -> bool;
}

/// Exhaustive production implementation of [`StaticLifecycleDispatch`].
pub struct ClosedOperationLifecycle;

impl StaticLifecycleDispatch for ClosedOperationLifecycle {
    type Context = RuntimeContext;
    type Cell = ExpandedOperationCell;
    type Prepared = crate::executors::PreparedOperation;
    type Invocation = crate::executors::OperationInvocation;
    type Outcome = OperationOutcome;

    async fn prepare(
        context: &Self::Context,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        executors::prepare_operation(context, cell).await
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        executors::operation_invocations(prepared, cell)
    }

    async fn invoke_one(context: &Self::Context, invocation: Self::Invocation) -> Self::Outcome {
        executors::invoke_operation(context, invocation).await
    }

    async fn verify(
        context: &Self::Context,
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[Self::Outcome],
    ) -> Result<Verification, ExecutorError> {
        executors::verify_operation(context, prepared, cell, outcomes).await
    }

    async fn teardown(
        context: &Self::Context,
        prepared: &mut Self::Prepared,
    ) -> crate::executors::TeardownResult {
        executors::teardown_operation(context, prepared).await
    }

    fn outcome_succeeded(outcome: &Self::Outcome) -> bool {
        outcome
            .response_metadata()
            .is_some_and(|metadata| metadata.status == ProductOutputStatus::Succeeded)
    }
}

pub struct StaticLifecycleDriver<D>(PhantomData<fn() -> D>);

impl<D> StaticLifecycleDriver<D>
where
    D: StaticLifecycleDispatch,
{
    pub async fn prepare(
        context: &D::Context,
        cell: &D::Cell,
    ) -> Result<D::Prepared, ExecutorError> {
        D::prepare(context, cell).await
    }

    pub fn validated_invocations(
        prepared: &D::Prepared,
        cell: &D::Cell,
        expected: u32,
    ) -> Result<Vec<D::Invocation>, StaticInvocationBuildError> {
        let invocations =
            D::invocations(prepared, cell).map_err(StaticInvocationBuildError::Generation)?;
        validate_invocation_batch(expected, invocations).map_err(StaticInvocationBuildError::Count)
    }

    pub async fn invoke_batch_with<O, N, F, Fut>(
        invocations: Vec<D::Invocation>,
        now_offset_ns: N,
        invoke: F,
    ) -> Vec<TimedLifecycleOutcome<O>>
    where
        N: Fn() -> u64 + Sync,
        F: Fn(D::Invocation) -> Fut + Sync,
        Fut: Future<Output = O>,
    {
        invoke_batch_at_shared_barrier(invocations, now_offset_ns, invoke).await
    }

    pub async fn invoke_one(context: &D::Context, invocation: D::Invocation) -> D::Outcome {
        D::invoke_one(context, invocation).await
    }

    pub async fn verify(
        context: &D::Context,
        prepared: &D::Prepared,
        cell: &D::Cell,
        outcomes: &[D::Outcome],
    ) -> Result<Verification, ExecutorError> {
        D::verify(context, prepared, cell, outcomes).await
    }

    pub async fn teardown(
        context: &D::Context,
        prepared: &mut D::Prepared,
    ) -> crate::executors::TeardownResult {
        D::teardown(context, prepared).await
    }

    pub fn outcome_succeeded(outcome: &D::Outcome) -> bool {
        D::outcome_succeeded(outcome)
    }
}

#[derive(Debug)]
pub struct TimedLifecycleOutcome<O> {
    pub request_id: String,
    pub outcome: O,
    pub start_offset_ns: u64,
    pub latency_ns: u64,
}

/// Releases a complete invocation batch from one shared barrier and records
/// monotonic timing around only the issued operation request.
pub async fn invoke_batch_at_shared_barrier<I, O, N, F, Fut>(
    invocations: Vec<I>,
    now_offset_ns: N,
    invoke: F,
) -> Vec<TimedLifecycleOutcome<O>>
where
    I: RuntimeInvocation,
    N: Fn() -> u64 + Sync,
    F: Fn(I) -> Fut + Sync,
    Fut: Future<Output = O>,
{
    let barrier = Arc::new(tokio::sync::Barrier::new(
        invocations.len().saturating_add(1),
    ));
    let futures = invocations.into_iter().map(|invocation| {
        let barrier = Arc::clone(&barrier);
        let request_id = invocation.request_id().to_owned();
        let invoke = &invoke;
        let now_offset_ns = &now_offset_ns;
        async move {
            barrier.wait().await;
            let start_offset_ns = now_offset_ns();
            let started = Instant::now();
            let outcome = invoke(invocation).await;
            TimedLifecycleOutcome {
                request_id,
                outcome,
                start_offset_ns,
                latency_ns: elapsed_ns(started),
            }
        }
    });
    let (_, outcomes) = tokio::join!(barrier.wait(), join_all(futures));
    outcomes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvocationCountMismatch {
    pub expected: u32,
    pub actual: usize,
}

#[derive(Debug)]
pub enum StaticInvocationBuildError {
    Generation(ExecutorError),
    Count(InvocationCountMismatch),
}

pub fn validate_invocation_batch<I>(
    expected: u32,
    invocations: Vec<I>,
) -> Result<Vec<I>, InvocationCountMismatch> {
    if usize::try_from(expected).ok() == Some(invocations.len()) {
        Ok(invocations)
    } else {
        Err(InvocationCountMismatch {
            expected,
            actual: invocations.len(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticRequestCompletion {
    Completed,
    CompletedAfterCancellation,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticRequestTiming {
    pub request_id: String,
    pub start_offset_ns: u64,
    pub latency_ns: u64,
    pub completion: StaticRequestCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticLifecycleIssue {
    InvocationGeneration(String),
    InvocationCount(InvocationCountMismatch),
    Verification(String),
    ResourceSampling(String),
    TeardownTimedOut,
}

#[derive(Debug, Clone, Copy)]
pub struct StaticLifecycleConfig {
    pub expected_invocations: u32,
    pub request_timeout: Duration,
    pub cancellation_grace: Duration,
    pub teardown_timeout: Duration,
    pub sampling_interval: SamplingInterval,
}

pub struct StaticLifecycleRun<O> {
    pub outcomes: Vec<O>,
    pub requests: Vec<StaticRequestTiming>,
    pub checks: Vec<CheckResult>,
    pub resources: Vec<ResourceReading>,
    pub lifecycle: LifecycleDurations,
    pub teardown: Option<crate::executors::TeardownResult>,
    pub issues: Vec<StaticLifecycleIssue>,
    pub barrier_participants: usize,
    pub product_succeeded: bool,
    pub cleanup_baseline_restored: bool,
    pub cancelled: bool,
    pub teardown_attempted: bool,
}

enum StaticInvocationCompletion<O> {
    Completed(O),
    CompletedAfterCancellation(O),
    TimedOut,
    Cancelled,
}

/// Drives a statically selected lifecycle through the production phase order.
/// Prepared state is always torn down, including count mismatch, verification
/// failure, request timeout, and cancellation paths.
pub async fn drive_static_lifecycle<D, C, N>(
    context: &D::Context,
    cell: &D::Cell,
    cancellation: &CancellationToken,
    config: StaticLifecycleConfig,
    collector: C,
    now: N,
) -> Result<StaticLifecycleRun<D::Outcome>, ExecutorError>
where
    D: StaticLifecycleDispatch,
    C: MetricCollector + 'static,
    N: Fn() -> MonotonicInstant + Clone + Send + Sync + 'static,
{
    let setup_started = Instant::now();
    let preparation = await_owned_task_with_grace(
        cancellation,
        None,
        config.cancellation_grace,
        StaticLifecycleDriver::<D>::prepare(context, cell),
    )
    .await;
    let mut prepared = match preparation {
        OwnedTaskOutcome::Completed(result) | OwnedTaskOutcome::CancelledCompleted(result) => {
            result?
        }
        OwnedTaskOutcome::TimedOut => {
            return Err(ExecutorError::InvalidRuntime(
                "static lifecycle preparation unexpectedly timed out",
            ));
        }
        OwnedTaskOutcome::CancelledBeforeStart => {
            return Err(ExecutorError::InvalidRuntime(
                "static lifecycle cancelled before preparation started",
            ));
        }
        OwnedTaskOutcome::CancelledAfterGrace => {
            return Err(ExecutorError::InvalidRuntime(
                "static lifecycle preparation exceeded cancellation grace before returning prepared state",
            ));
        }
    };

    let mut issues = Vec::new();
    let invocations = match StaticLifecycleDriver::<D>::validated_invocations(
        &prepared,
        cell,
        config.expected_invocations,
    ) {
        Ok(invocations) => invocations,
        Err(StaticInvocationBuildError::Generation(error)) => {
            issues.push(StaticLifecycleIssue::InvocationGeneration(
                error.to_string(),
            ));
            Vec::new()
        }
        Err(StaticInvocationBuildError::Count(mismatch)) => {
            issues.push(StaticLifecycleIssue::InvocationCount(mismatch));
            Vec::new()
        }
    };
    let setup_ns = elapsed_ns(setup_started);

    let operation_started = Instant::now();
    let barrier_participants = invocations.len();
    let mut resources = Vec::new();
    let timed = if issues.is_empty() && !cancellation.is_cancelled() {
        let sampler_now = now.clone();
        let sampler = MetricSamplingTask::start(config.sampling_interval, collector, sampler_now);
        let request_now = now.clone();
        let timed = StaticLifecycleDriver::<D>::invoke_batch_with(
            invocations,
            move || request_now().offset_ns(),
            |invocation| async {
                match await_owned_task_with_grace(
                    cancellation,
                    Some(config.request_timeout),
                    config.cancellation_grace,
                    StaticLifecycleDriver::<D>::invoke_one(context, invocation),
                )
                .await
                {
                    OwnedTaskOutcome::Completed(outcome) => {
                        StaticInvocationCompletion::Completed(outcome)
                    }
                    OwnedTaskOutcome::CancelledCompleted(outcome) => {
                        StaticInvocationCompletion::CompletedAfterCancellation(outcome)
                    }
                    OwnedTaskOutcome::TimedOut => StaticInvocationCompletion::TimedOut,
                    OwnedTaskOutcome::CancelledBeforeStart
                    | OwnedTaskOutcome::CancelledAfterGrace => {
                        StaticInvocationCompletion::Cancelled
                    }
                }
            },
        )
        .await;
        match sampler.finish().await {
            Ok(readings) => resources = readings,
            Err(error) => issues.push(StaticLifecycleIssue::ResourceSampling(error.to_string())),
        }
        timed
    } else {
        Vec::new()
    };
    let operation_ns = elapsed_ns(operation_started);

    let mut outcomes = Vec::new();
    let mut requests = Vec::with_capacity(timed.len());
    for timed in timed {
        let completion = match timed.outcome {
            StaticInvocationCompletion::Completed(outcome) => {
                outcomes.push(outcome);
                StaticRequestCompletion::Completed
            }
            StaticInvocationCompletion::CompletedAfterCancellation(outcome) => {
                outcomes.push(outcome);
                StaticRequestCompletion::CompletedAfterCancellation
            }
            StaticInvocationCompletion::TimedOut => StaticRequestCompletion::TimedOut,
            StaticInvocationCompletion::Cancelled => StaticRequestCompletion::Cancelled,
        };
        requests.push(StaticRequestTiming {
            request_id: timed.request_id,
            start_offset_ns: timed.start_offset_ns,
            latency_ns: timed.latency_ns,
            completion,
        });
    }

    let product_succeeded = outcomes.len()
        == usize::try_from(config.expected_invocations).unwrap_or(usize::MAX)
        && requests
            .iter()
            .all(|request| request.completion == StaticRequestCompletion::Completed)
        && outcomes
            .iter()
            .all(StaticLifecycleDriver::<D>::outcome_succeeded);

    let verify_started = Instant::now();
    let mut checks = Vec::new();
    if issues.is_empty() && !cancellation.is_cancelled() && !outcomes.is_empty() {
        match await_owned_task_with_grace(
            cancellation,
            None,
            config.cancellation_grace,
            StaticLifecycleDriver::<D>::verify(context, &prepared, cell, &outcomes),
        )
        .await
        {
            OwnedTaskOutcome::Completed(Ok(verification))
            | OwnedTaskOutcome::CancelledCompleted(Ok(verification)) => {
                checks.extend(verification.checks)
            }
            OwnedTaskOutcome::Completed(Err(error))
            | OwnedTaskOutcome::CancelledCompleted(Err(error)) => {
                issues.push(StaticLifecycleIssue::Verification(error.to_string()));
            }
            OwnedTaskOutcome::TimedOut => issues.push(StaticLifecycleIssue::Verification(
                "verification unexpectedly timed out".to_owned(),
            )),
            OwnedTaskOutcome::CancelledBeforeStart => {}
            OwnedTaskOutcome::CancelledAfterGrace => {
                issues.push(StaticLifecycleIssue::Verification(
                    "verification exceeded cancellation grace".to_owned(),
                ))
            }
        }
    }
    let verify_ns = elapsed_ns(verify_started);

    let teardown_started = Instant::now();
    let teardown_attempted = true;
    let teardown = match tokio::time::timeout(
        config.teardown_timeout,
        StaticLifecycleDriver::<D>::teardown(context, &mut prepared),
    )
    .await
    {
        Ok(teardown) => {
            checks.extend(teardown.checks.clone());
            Some(teardown)
        }
        Err(_) => {
            issues.push(StaticLifecycleIssue::TeardownTimedOut);
            None
        }
    };
    let teardown_ns = elapsed_ns(teardown_started);
    let cleanup_baseline_restored = teardown
        .as_ref()
        .is_some_and(|teardown| teardown.baseline_restored && teardown.errors.is_empty());

    Ok(StaticLifecycleRun {
        outcomes,
        requests,
        checks,
        resources,
        lifecycle: LifecycleDurations {
            setup_ns,
            operation_ns,
            verify_ns,
            teardown_ns,
        },
        teardown,
        issues,
        barrier_participants,
        product_succeeded,
        cleanup_baseline_restored,
        cancelled: cancellation.is_cancelled(),
        teardown_attempted,
    })
}

type TimedOperationOutcome = TimedLifecycleOutcome<OperationOutcome>;

#[derive(Debug)]
struct TrialExecutionResult {
    control: CampaignControl,
    abort: Option<SchedulerError>,
}

#[derive(Debug)]
enum SetupCancellationCleanup {
    RootOnly,
    OwnedSandbox(OwnedSandboxId),
    DeferredToGatewayOrCellOwner,
}

impl TrialExecutionResult {
    const fn continue_campaign() -> Self {
        Self {
            control: CampaignControl::Continue,
            abort: None,
        }
    }

    const fn cancelled() -> Self {
        Self {
            control: CampaignControl::Cancelled,
            abort: None,
        }
    }

    fn abort(error: SchedulerError) -> Self {
        Self {
            control: CampaignControl::Continue,
            abort: Some(error),
        }
    }
}

struct ResourceSampler {
    task: MetricSamplingTask,
}

impl ResourceSampler {
    fn start(interval: SamplingInterval, clock: Arc<MonotonicClock>) -> Self {
        let collector = ProcessCollector::attach(ProcessScope::Runner, std::process::id());
        let task = MetricSamplingTask::start(interval, collector, move || {
            MonotonicInstant::from_offset_ns(clock.offset_ns())
        });
        Self { task }
    }

    async fn finish(self) -> Result<Vec<ResourceReading>, SchedulerError> {
        self.task
            .finish()
            .await
            .map_err(|error| SchedulerError::ResourceTask(error.to_string()))
    }
}

#[derive(Debug, Clone)]
struct ProductResourceQuerySample {
    monotonic_offset_ns: u64,
    sampled: bool,
    resources: Result<ProductSandboxResources, String>,
    storage: Result<ProductStorageResources, String>,
}

#[derive(Debug)]
struct ProductResourceSampler {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Vec<ProductResourceQuerySample>>,
}

impl ProductResourceSampler {
    fn start(
        interval: SamplingInterval,
        clock: Arc<MonotonicClock>,
        product: Arc<ProductGateway>,
        sandbox_id: OwnedSandboxId,
        run_id: String,
        cell_id: String,
        trial_id: String,
    ) -> Self {
        let stop = CancellationToken::new();
        let sampler_stop = stop.clone();
        let task = tokio::spawn(async move {
            let mut samples = Vec::new();
            let mut sequence = 0_u64;
            let mut tick = tokio::time::interval(interval.as_duration());
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The pre-barrier baseline is collected synchronously.
            tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = sampler_stop.cancelled() => break,
                    _ = tick.tick() => {
                        let label = format!("resources-sample-{sequence:08}");
                        sequence = sequence.saturating_add(1);
                        samples.push(observe_product_resources(
                            &product,
                            &sandbox_id,
                            &run_id,
                            &cell_id,
                            &trial_id,
                            &label,
                            true,
                            &clock,
                        ).await);
                    }
                }
            }
            samples
        });
        Self { stop, task }
    }

    async fn finish(self) -> Result<Vec<ProductResourceQuerySample>, SchedulerError> {
        self.stop.cancel();
        self.task
            .await
            .map_err(|error| SchedulerError::ResourceTask(error.to_string()))
    }
}

#[allow(clippy::too_many_arguments)]
async fn observe_product_resources(
    product: &ProductGateway,
    sandbox_id: &OwnedSandboxId,
    run_id: &str,
    cell_id: &str,
    trial_id: &str,
    label: &str,
    sampled: bool,
    clock: &MonotonicClock,
) -> ProductResourceQuerySample {
    let monotonic_offset_ns = clock.offset_ns();
    let (resources, storage) = match (
        Correlation::new(run_id, cell_id, trial_id, format!("{label}-cgroup")),
        Correlation::new(run_id, cell_id, trial_id, format!("{label}-storage")),
    ) {
        (Ok(cgroup_correlation), Ok(storage_correlation)) => {
            // The typed snapshot performs the checked synchronous telemetry
            // refresh. It must complete before the cgroup view reads the log,
            // otherwise a concurrent cgroup query could select the previous
            // baseline and mislabel it as the current boundary sample.
            // Both requests share one deadline so ordering does not double the
            // sampler's bounded shutdown/cancellation delay.
            let deadline = tokio::time::Instant::now() + PRODUCT_RESOURCE_OBSERVE_TIMEOUT;
            let storage = match tokio::time::timeout_at(
                deadline,
                product.observe_storage_resources(sandbox_id, &storage_correlation),
            )
            .await
            {
                Ok(Ok(storage)) => Ok(storage),
                Ok(Err(error)) => Err(bounded_event_text(&error.to_string())),
                Err(_) => Err("sandbox storage observation timed out".to_owned()),
            };
            let resources = match &storage {
                Ok(_) => match tokio::time::timeout_at(
                    deadline,
                    product.observe_sandbox_resources(sandbox_id, &cgroup_correlation),
                )
                .await
                {
                    Ok(Ok(resources)) => Ok(resources),
                    Ok(Err(error)) => Err(bounded_event_text(&error.to_string())),
                    Err(_) => Err("sandbox resource observation timed out".to_owned()),
                },
                Err(reason) => Err(bounded_event_text(&format!(
                    "sandbox resource observation skipped because the fresh snapshot was unavailable: {reason}"
                ))),
            };
            (resources, storage)
        }
        (Err(error), _) | (_, Err(error)) => {
            let reason = bounded_event_text(&error.to_string());
            (Err(reason.clone()), Err(reason))
        }
    };
    ProductResourceQuerySample {
        monotonic_offset_ns,
        sampled,
        resources,
        storage,
    }
}

fn product_resource_readings(sample: ProductResourceQuerySample) -> Vec<ResourceReading> {
    let definitions = [
        SANDBOX_MEMORY_CURRENT,
        SANDBOX_MEMORY_PEAK,
        SANDBOX_CPU_TIME,
        SANDBOX_BLOCK_READ,
        SANDBOX_BLOCK_WRITE,
    ];
    let mut readings = match sample.resources {
        Ok(resources) => {
            let memory = product_optional_counter(
                resources.memory_current_bytes,
                "memory.current was not reported by the sandbox runtime",
            );
            let cpu = match resources.cpu_usage_usec {
                Some(value) => value.checked_mul(1_000).map_or_else(
                    || Availability::Unavailable {
                        source: PRODUCT_RESOURCE_SOURCE.to_owned(),
                        reason: "CPU counter overflow while converting microseconds to nanoseconds"
                            .to_owned(),
                    },
                    |value| Availability::Available { value },
                ),
                None => product_optional_counter(
                    None,
                    "CPU usage counter was not reported by the sandbox runtime",
                ),
            };
            vec![
                product_resource_reading(
                    &SANDBOX_MEMORY_CURRENT,
                    sample.monotonic_offset_ns,
                    sample.sampled,
                    "memory.current",
                    memory.clone(),
                ),
                product_resource_reading(
                    &SANDBOX_MEMORY_PEAK,
                    sample.monotonic_offset_ns,
                    true,
                    "memory.current.sampled_peak",
                    memory,
                ),
                product_resource_reading(
                    &SANDBOX_CPU_TIME,
                    sample.monotonic_offset_ns,
                    sample.sampled,
                    "cpu.usage_usec",
                    cpu,
                ),
                product_resource_reading(
                    &SANDBOX_BLOCK_READ,
                    sample.monotonic_offset_ns,
                    sample.sampled,
                    "io.read_bytes",
                    product_optional_counter(
                        resources.io_read_bytes,
                        "block-I/O read counter was not reported by the sandbox runtime",
                    ),
                ),
                product_resource_reading(
                    &SANDBOX_BLOCK_WRITE,
                    sample.monotonic_offset_ns,
                    sample.sampled,
                    "io.write_bytes",
                    product_optional_counter(
                        resources.io_write_bytes,
                        "block-I/O write counter was not reported by the sandbox runtime",
                    ),
                ),
            ]
        }
        Err(reason) => definitions
            .iter()
            .map(|definition| {
                product_resource_reading(
                    definition,
                    sample.monotonic_offset_ns,
                    sample.sampled,
                    "query",
                    Availability::Unavailable {
                        source: PRODUCT_RESOURCE_SOURCE.to_owned(),
                        reason: reason.clone(),
                    },
                )
            })
            .collect(),
    };
    readings.extend(product_storage_readings(
        sample.storage,
        sample.monotonic_offset_ns,
        sample.sampled,
    ));
    readings
}

fn product_storage_readings(
    storage: Result<ProductStorageResources, String>,
    monotonic_offset_ns: u64,
    sampled: bool,
) -> Vec<ResourceReading> {
    let definitions = [
        DAEMON_RSS,
        DAEMON_CPU_TIME,
        LAYERSTACK_BYTES,
        UPPERDIR_BYTES,
    ];
    match storage {
        Ok(storage) => {
            let daemon_reason = format!(
                "product snapshot exposed container PID {} without a host PID namespace and process start identity",
                storage.daemon_container_pid
            );
            let daemon_unavailable = || Availability::Unavailable {
                source: format!("{PRODUCT_STORAGE_SOURCE}.daemon.daemon_pid"),
                reason: daemon_reason.clone(),
            };
            let layerstack = storage.layerstack_storage_allocated_bytes.map_or_else(
                || Availability::Unavailable {
                    source: format!("{PRODUCT_STORAGE_SOURCE}.stack.storage_allocated_bytes"),
                    reason: "LayerStack allocated storage was not reported by the product"
                        .to_owned(),
                },
                |value| Availability::Available { value },
            );
            let upperdir = match storage.upperdir {
                ProductUpperdirAllocation::Available {
                    allocated_bytes,
                    workspace_count: _,
                } => Availability::Available {
                    value: allocated_bytes,
                },
                ProductUpperdirAllocation::Unavailable { reason } => Availability::Unavailable {
                    source: format!("{PRODUCT_STORAGE_SOURCE}.workspaces.disk_allocated_bytes"),
                    reason,
                },
            };
            vec![
                snapshot_resource_reading(
                    &DAEMON_RSS,
                    monotonic_offset_ns,
                    sampled,
                    "daemon.daemon_pid",
                    daemon_unavailable(),
                ),
                snapshot_resource_reading(
                    &DAEMON_CPU_TIME,
                    monotonic_offset_ns,
                    sampled,
                    "daemon.daemon_pid",
                    daemon_unavailable(),
                ),
                snapshot_resource_reading(
                    &LAYERSTACK_BYTES,
                    monotonic_offset_ns,
                    sampled,
                    "stack.storage_allocated_bytes",
                    layerstack,
                ),
                snapshot_resource_reading(
                    &UPPERDIR_BYTES,
                    monotonic_offset_ns,
                    sampled,
                    "workspaces.disk_allocated_bytes.sum",
                    upperdir,
                ),
            ]
        }
        Err(reason) => definitions
            .iter()
            .map(|definition| {
                snapshot_resource_reading(
                    definition,
                    monotonic_offset_ns,
                    sampled,
                    "query",
                    Availability::Unavailable {
                        source: PRODUCT_STORAGE_SOURCE.to_owned(),
                        reason: reason.clone(),
                    },
                )
            })
            .collect(),
    }
}

fn product_optional_counter(value: Option<u64>, reason: &str) -> Availability<u64> {
    value.map_or_else(
        || Availability::Unavailable {
            source: PRODUCT_RESOURCE_SOURCE.to_owned(),
            reason: reason.to_owned(),
        },
        |value| Availability::Available { value },
    )
}

fn product_resource_reading(
    definition: &MetricDefinition,
    monotonic_offset_ns: u64,
    sampled: bool,
    field: &str,
    value: Availability<u64>,
) -> ResourceReading {
    ResourceReading {
        schema_version: 1,
        metric_id: definition.id.to_owned(),
        metric_semantic_revision: definition.semantic_revision,
        unit: definition.unit,
        scope: definition.scope,
        kind: definition.kind,
        aggregation: definition.aggregation,
        source: format!("{PRODUCT_RESOURCE_SOURCE}.{field}"),
        monotonic_offset_ns,
        sampled,
        value: value.map(|value| value as f64),
    }
}

fn snapshot_resource_reading(
    definition: &MetricDefinition,
    monotonic_offset_ns: u64,
    sampled: bool,
    field: &str,
    value: Availability<u64>,
) -> ResourceReading {
    ResourceReading {
        schema_version: 1,
        metric_id: definition.id.to_owned(),
        metric_semantic_revision: definition.semantic_revision,
        unit: definition.unit,
        scope: definition.scope,
        kind: definition.kind,
        aggregation: definition.aggregation,
        source: format!("{PRODUCT_STORAGE_SOURCE}.{field}"),
        monotonic_offset_ns,
        sampled,
        value: value.map(|value| value as f64),
    }
}

#[derive(Debug, Clone)]
struct LayerstackQuerySample {
    monotonic_offset_ns: u64,
    sampled: bool,
    snapshot: Result<ProductLayerstackSnapshot, String>,
}

#[derive(Debug)]
struct LayerstackSampler {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Vec<LayerstackQuerySample>>,
}

impl LayerstackSampler {
    fn start(
        interval: SamplingInterval,
        clock: Arc<MonotonicClock>,
        product: Arc<ProductGateway>,
        sandbox_id: OwnedSandboxId,
        run_id: String,
        cell_id: String,
        trial_id: String,
    ) -> Self {
        let stop = CancellationToken::new();
        let sampler_stop = stop.clone();
        let task = tokio::spawn(async move {
            let mut samples = Vec::new();
            let mut sequence = 0_u64;
            let mut tick = tokio::time::interval(interval.as_duration());
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // S0 is collected synchronously before this sampler starts.
            tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = sampler_stop.cancelled() => break,
                    _ = tick.tick() => {
                        let label = format!("layerstack-sample-{sequence:08}");
                        sequence = sequence.saturating_add(1);
                        samples.push(observe_layerstack_query(
                            &product,
                            &sandbox_id,
                            &run_id,
                            &cell_id,
                            &trial_id,
                            &label,
                            true,
                            &clock,
                        ).await);
                    }
                }
            }
            samples
        });
        Self { stop, task }
    }

    async fn finish(self) -> Result<Vec<LayerstackQuerySample>, SchedulerError> {
        self.stop.cancel();
        self.task
            .await
            .map_err(|error| SchedulerError::ResourceTask(error.to_string()))
    }
}

#[allow(clippy::too_many_arguments)]
async fn observe_layerstack_query(
    product: &ProductGateway,
    sandbox_id: &OwnedSandboxId,
    run_id: &str,
    cell_id: &str,
    trial_id: &str,
    label: &str,
    sampled: bool,
    clock: &MonotonicClock,
) -> LayerstackQuerySample {
    let monotonic_offset_ns = clock.offset_ns();
    let snapshot = match Correlation::new(run_id, cell_id, trial_id, label) {
        Ok(correlation) => match tokio::time::timeout(
            LAYERSTACK_OBSERVE_TIMEOUT,
            product.observe_layerstack(sandbox_id, &correlation),
        )
        .await
        {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(error)) => Err(bounded_event_text(&error.to_string())),
            Err(_) => Err("layerstack observation timed out".to_owned()),
        },
        Err(error) => Err(bounded_event_text(&error.to_string())),
    };
    LayerstackQuerySample {
        monotonic_offset_ns,
        sampled,
        snapshot,
    }
}

fn unavailable<T>(source: &str, reason: impl Into<String>) -> Availability<T> {
    Availability::Unavailable {
        source: source.to_owned(),
        reason: reason.into(),
    }
}

fn product_counter(value: Option<u64>, field: &str) -> Availability<u64> {
    value.map_or_else(
        || {
            unavailable(
                LAYERSTACK_OBSERVATION_SOURCE,
                format!("{field} unavailable"),
            )
        },
        |value| Availability::Available { value },
    )
}

fn storage_snapshot(sample: &LayerstackQuerySample) -> executors::layerstack::StorageSnapshot {
    let offset = Availability::Available {
        value: sample.monotonic_offset_ns,
    };
    match &sample.snapshot {
        Ok(snapshot) => executors::layerstack::StorageSnapshot {
            monotonic_offset_ns: offset,
            sampled: sample.sampled,
            manifest_version: Availability::Available {
                value: snapshot.manifest_version,
            },
            root_hash: Availability::Available {
                value: snapshot.root_hash.clone(),
            },
            active_layer_count: Availability::Available {
                value: u64::try_from(snapshot.layers.len()).unwrap_or(u64::MAX),
            },
            active_lease_count: Availability::Available {
                value: u64::from(snapshot.active_lease_count),
            },
            active_logical_bytes: product_counter(snapshot.total_bytes, "total_bytes"),
            active_allocated_bytes: product_counter(
                snapshot.total_allocated_bytes,
                "total_allocated_bytes",
            ),
            storage_logical_bytes: product_counter(
                snapshot.storage_logical_bytes,
                "storage_logical_bytes",
            ),
            storage_allocated_bytes: product_counter(
                snapshot.storage_allocated_bytes,
                "storage_allocated_bytes",
            ),
            staging_entry_count: product_counter(
                snapshot.staging_entry_count,
                "staging_entry_count",
            ),
        },
        Err(reason) => unavailable_storage_snapshot(
            offset,
            sample.sampled,
            LAYERSTACK_OBSERVATION_SOURCE,
            reason,
        ),
    }
}

fn unavailable_storage_snapshot(
    monotonic_offset_ns: Availability<u64>,
    sampled: bool,
    source: &str,
    reason: &str,
) -> executors::layerstack::StorageSnapshot {
    executors::layerstack::StorageSnapshot {
        monotonic_offset_ns,
        sampled,
        manifest_version: unavailable(source, reason),
        root_hash: unavailable(source, reason),
        active_layer_count: unavailable(source, reason),
        active_lease_count: unavailable(source, reason),
        active_logical_bytes: unavailable(source, reason),
        active_allocated_bytes: unavailable(source, reason),
        storage_logical_bytes: unavailable(source, reason),
        storage_allocated_bytes: unavailable(source, reason),
        staging_entry_count: unavailable(source, reason),
    }
}

async fn settle_layerstack(
    product: &ProductGateway,
    sandbox_id: &OwnedSandboxId,
    run_id: &str,
    cell_id: &str,
    trial_id: &str,
    clock: &MonotonicClock,
) -> (
    LayerstackQuerySample,
    Vec<LayerstackQuerySample>,
    Option<String>,
) {
    let deadline = tokio::time::Instant::now() + LAYERSTACK_SETTLE_TIMEOUT;
    let mut samples = Vec::new();
    let mut previous = None;
    let mut consecutive = 0_usize;
    let mut sequence = 0_u64;
    loop {
        let label = format!("layerstack-settle-{sequence:08}");
        sequence = sequence.saturating_add(1);
        let sample = observe_layerstack_query(
            product, sandbox_id, run_id, cell_id, trial_id, &label, false, clock,
        )
        .await;
        if let Ok(snapshot) = &sample.snapshot {
            if previous.as_ref() == Some(snapshot) {
                consecutive = consecutive.saturating_add(1);
            } else {
                previous = Some(snapshot.clone());
                consecutive = 1;
            }
        } else {
            previous = None;
            consecutive = 0;
        }
        samples.push(sample.clone());
        if consecutive >= LAYERSTACK_SETTLE_MATCHES {
            return (sample, samples, None);
        }
        if tokio::time::Instant::now() >= deadline {
            let unavailable = LayerstackQuerySample {
                monotonic_offset_ns: clock.offset_ns(),
                sampled: false,
                snapshot: Err("post-sweep LayerStack state did not stabilize".to_owned()),
            };
            return (
                unavailable,
                samples,
                Some(
                    "post-sweep LayerStack state did not stabilize before the fixed deadline"
                        .to_owned(),
                ),
            );
        }
        tokio::time::sleep(LAYERSTACK_SETTLE_INTERVAL).await;
    }
}

async fn observe_layerstack_trace(
    product: &ProductGateway,
    sandbox_id: &OwnedSandboxId,
    run_id: &str,
    cell_id: &str,
    trial_id: &str,
) -> Result<ProductTrace, String> {
    let target = Correlation::new(run_id, cell_id, trial_id, LAYERSTACK_REQUEST_ID)
        .map_err(|error| bounded_event_text(&error.to_string()))?;
    let deadline = tokio::time::Instant::now() + LAYERSTACK_TRACE_RETRY_TIMEOUT;
    let mut sequence = 0_u64;
    loop {
        let query = Correlation::new(
            run_id,
            cell_id,
            trial_id,
            format!("layerstack-trace-{sequence:08}"),
        )
        .map_err(|error| bounded_event_text(&error.to_string()))?;
        sequence = sequence.saturating_add(1);
        let last_error = match tokio::time::timeout(
            LAYERSTACK_OBSERVE_TIMEOUT,
            product.observe_trace(sandbox_id, &target, &query),
        )
        .await
        {
            Ok(Ok(trace)) => return Ok(trace),
            Ok(Err(error)) => bounded_event_text(&error.to_string()),
            Err(_) => "trace observation timed out".to_owned(),
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }
        tokio::time::sleep(LAYERSTACK_TRACE_RETRY_INTERVAL).await;
    }
}

#[derive(Debug)]
struct LayerstackTraceCollection {
    phases: Vec<PhaseObservation>,
    s2_post_commit: executors::layerstack::StorageSnapshot,
    warnings: Vec<String>,
}

fn collect_layerstack_trace(
    trace: Result<&ProductTrace, &str>,
    cell_id: &str,
    trial_id: &str,
    request_start_offset_ns: Option<u64>,
    expected_live_sessions: u32,
) -> LayerstackTraceCollection {
    match trace {
        Ok(trace) => collect_available_layerstack_trace(
            trace,
            cell_id,
            trial_id,
            request_start_offset_ns,
            expected_live_sessions,
        ),
        Err(trace_error) => unavailable_layerstack_trace_collection(
            vec![format!("LayerStack trace unavailable: {trace_error}")],
            trace_error,
        ),
    }
}

fn unavailable_layerstack_trace_collection(
    warnings: Vec<String>,
    reason: &str,
) -> LayerstackTraceCollection {
    LayerstackTraceCollection {
        phases: Vec::new(),
        s2_post_commit: unavailable_storage_snapshot(
            unavailable(
                LAYERSTACK_TRACE_SOURCE,
                "commit-boundary monotonic offset unavailable",
            ),
            false,
            LAYERSTACK_TRACE_SOURCE,
            reason,
        ),
        warnings,
    }
}

fn collect_available_layerstack_trace(
    trace: &ProductTrace,
    cell_id: &str,
    trial_id: &str,
    request_start_offset_ns: Option<u64>,
    expected_live_sessions: u32,
) -> LayerstackTraceCollection {
    let mut warnings = Vec::new();
    let [dispatch] = trace.spans.as_slice() else {
        warnings.push(format!(
            "trace {} contained {} roots; exactly one daemon.dispatch root is required",
            trace.trace_id,
            trace.spans.len()
        ));
        return unavailable_layerstack_trace_collection(
            warnings,
            "exact daemon.dispatch root was not correlated",
        );
    };
    if dispatch.span.name != "daemon.dispatch" {
        warnings.push(format!(
            "trace {} root was {}; daemon.dispatch is required",
            trace.trace_id, dispatch.span.name
        ));
        return unavailable_layerstack_trace_collection(
            warnings,
            "exact daemon.dispatch root was not correlated",
        );
    }
    let squash_nodes = dispatch
        .children
        .iter()
        .filter(|node| node.span.name == "layerstack.squash")
        .collect::<Vec<_>>();
    let Some(squash) = (squash_nodes.len() == 1).then_some(squash_nodes[0]) else {
        warnings.push(format!(
            "trace {} contained {} layerstack.squash spans; exactly one is required",
            trace.trace_id,
            squash_nodes.len()
        ));
        return LayerstackTraceCollection {
            phases: Vec::new(),
            s2_post_commit: unavailable_storage_snapshot(
                unavailable(
                    LAYERSTACK_TRACE_SOURCE,
                    "commit-boundary monotonic offset unavailable",
                ),
                false,
                LAYERSTACK_TRACE_SOURCE,
                "exact squash span was not correlated",
            ),
            warnings,
        };
    };

    let sweep_nodes = squash
        .children
        .iter()
        .filter(|node| node.span.name == "layerstack.squash.remount_sweep")
        .collect::<Vec<_>>();
    let phases = crate::definitions::definition(OperationId::SquashLayerstack).phases;
    let mut observations = Vec::new();
    for phase in phases {
        let nodes = match phase.id {
            PhaseId::LayerstackSquash => vec![squash],
            PhaseId::WorkspaceSessionRemount => {
                sweep_nodes.first().map_or_else(Vec::new, |sweep| {
                    sweep
                        .children
                        .iter()
                        .filter(|node| node.span.name == phase.trace_span_name)
                        .collect()
                })
            }
            PhaseId::LayerstackStoragePlan
            | PhaseId::LayerstackFlatten
            | PhaseId::LayerstackCommit
            | PhaseId::LayerstackRemountSweep => squash
                .children
                .iter()
                .filter(|node| node.span.name == phase.trace_span_name)
                .collect(),
        };
        let expected = match phase.id {
            PhaseId::WorkspaceSessionRemount => {
                usize::try_from(expected_live_sessions).unwrap_or(usize::MAX)
            }
            PhaseId::LayerstackSquash
            | PhaseId::LayerstackStoragePlan
            | PhaseId::LayerstackFlatten
            | PhaseId::LayerstackCommit
            | PhaseId::LayerstackRemountSweep => 1,
        };
        if nodes.len() != expected {
            warnings.push(format!(
                "phase {:?} matched {} spans; expected {expected}",
                phase.id,
                nodes.len()
            ));
            if phase.id != PhaseId::WorkspaceSessionRemount {
                continue;
            }
        }
        for node in nodes {
            let Some(request_start) = request_start_offset_ns else {
                warnings.push(format!(
                    "phase {:?} omitted because the request start offset is unavailable",
                    phase.id
                ));
                break;
            };
            let start = milliseconds_to_nanoseconds(node.offset_ms).and_then(|offset| {
                request_start
                    .checked_add(offset)
                    .ok_or("phase start overflow")
            });
            let duration = milliseconds_to_nanoseconds(node.span.dur_ms);
            match (start, duration) {
                (Ok(start_offset_ns), Ok(duration_ns)) => {
                    observations.push(PhaseObservation {
                        id: phase.id,
                        semantic_revision: phase.semantic_revision,
                        unit: phase.unit,
                        cell_id: cell_id.to_owned(),
                        trial_id: trial_id.to_owned(),
                        request_id: Some(LAYERSTACK_REQUEST_ID.to_owned()),
                        source: phase.source,
                        correlation: phase.correlation,
                        trace_span_name: phase.trace_span_name.to_owned(),
                        start_offset_ns,
                        duration_ns,
                        status: phase_status(node.span.status),
                    });
                }
                (Err(reason), _) | (_, Err(reason)) => warnings.push(format!(
                    "phase {:?} omitted because its product time was invalid: {reason}",
                    phase.id
                )),
            }
        }
    }

    let commit = squash
        .children
        .iter()
        .filter(|node| node.span.name == "layerstack.squash.commit")
        .collect::<Vec<_>>();
    let s2_offset = match (request_start_offset_ns, commit.as_slice()) {
        (Some(request_start), [commit]) => milliseconds_to_nanoseconds(commit.offset_ms)
            .and_then(|offset| {
                milliseconds_to_nanoseconds(commit.span.dur_ms)
                    .and_then(|duration| offset.checked_add(duration).ok_or("S2 offset overflow"))
            })
            .and_then(|offset| {
                request_start
                    .checked_add(offset)
                    .ok_or("S2 monotonic offset overflow")
            })
            .map_or_else(
                |reason| unavailable(LAYERSTACK_TRACE_SOURCE, reason),
                |value| Availability::Available { value },
            ),
        (None, _) => unavailable(LAYERSTACK_TRACE_SOURCE, "request start offset unavailable"),
        (_, _) => unavailable(
            LAYERSTACK_TRACE_SOURCE,
            "exact commit span was not correlated",
        ),
    };
    let s2_post_commit = executors::layerstack::StorageSnapshot {
        monotonic_offset_ns: s2_offset,
        sampled: false,
        manifest_version: trace_u64_attr(squash, "manifest_version"),
        root_hash: trace_string_attr(squash, "s2_root_hash"),
        active_layer_count: trace_u64_attr(squash, "s2_layer_count"),
        active_lease_count: unavailable(
            LAYERSTACK_TRACE_SOURCE,
            "active lease count is not emitted at the commit boundary",
        ),
        active_logical_bytes: trace_u64_attr(squash, "s2_active_logical_bytes"),
        active_allocated_bytes: trace_u64_attr(squash, "s2_active_allocated_bytes"),
        storage_logical_bytes: trace_u64_attr(squash, "s2_storage_logical_bytes"),
        storage_allocated_bytes: trace_u64_attr(squash, "s2_storage_allocated_bytes"),
        staging_entry_count: trace_u64_attr(squash, "s2_staging_entry_count"),
    };
    LayerstackTraceCollection {
        phases: observations,
        s2_post_commit,
        warnings,
    }
}

fn milliseconds_to_nanoseconds(value: f64) -> Result<u64, &'static str> {
    if !value.is_finite() || value < 0.0 {
        return Err("milliseconds must be finite and non-negative");
    }
    let nanoseconds = value * 1_000_000.0;
    // `u64::MAX as f64` rounds to 2^64, so equality is already outside the
    // representable integer range and must be rejected before the saturating
    // float-to-integer cast.
    if !nanoseconds.is_finite() || nanoseconds >= 18_446_744_073_709_551_616.0 {
        return Err("milliseconds overflow nanoseconds");
    }
    Ok(nanoseconds.round() as u64)
}

fn phase_status(status: ProductTraceStatus) -> PhaseStatus {
    match status {
        ProductTraceStatus::Completed => PhaseStatus::Succeeded,
        ProductTraceStatus::Error => PhaseStatus::Failed,
        ProductTraceStatus::Cancelled => PhaseStatus::Cancelled,
        ProductTraceStatus::TimedOut => PhaseStatus::TimedOut,
    }
}

fn trace_u64_attr(node: &ProductTraceSpanNode, key: &str) -> Availability<u64> {
    node.span
        .attrs
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map_or_else(
            || {
                unavailable(
                    LAYERSTACK_TRACE_SOURCE,
                    format!("trace attribute {key} unavailable"),
                )
            },
            |value| Availability::Available { value },
        )
}

fn trace_string_attr(node: &ProductTraceSpanNode, key: &str) -> Availability<String> {
    node.span
        .attrs
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                unavailable(
                    LAYERSTACK_TRACE_SOURCE,
                    format!("trace attribute {key} unavailable"),
                )
            },
            |value| Availability::Available {
                value: value.to_owned(),
            },
        )
}

fn collect_layerstack_evidence(
    partial: &executors::layerstack::SquashLayerstackPartialEvidence,
    effective_remount_parallelism: u32,
    s0: &LayerstackQuerySample,
    operation_samples: &[LayerstackQuerySample],
    s2_post_commit: executors::layerstack::StorageSnapshot,
    s3: &LayerstackQuerySample,
) -> executors::layerstack::SquashLayerstackCollectedEvidence {
    let s0_baseline = storage_snapshot(s0);
    let s3_settled = storage_snapshot(s3);
    let source_layer_allocations = source_layer_allocations(partial, s0);
    let reclaimed_bytes = reclaimed_bytes(partial, &source_layer_allocations);
    let mut peak_candidates = Vec::with_capacity(operation_samples.len().saturating_add(3));
    peak_candidates.push(s0_baseline.clone());
    peak_candidates.extend(operation_samples.iter().map(storage_snapshot));
    peak_candidates.push(s2_post_commit.clone());
    peak_candidates.push(s3_settled.clone());
    let s1_sampled_peak = sampled_peak(&peak_candidates);
    executors::layerstack::SquashLayerstackCollectedEvidence {
        effective_remount_parallelism,
        source_layer_allocations,
        reclaimed_bytes,
        s0_baseline,
        s1_sampled_peak,
        s2_post_commit,
        s3_settled,
    }
}

fn source_layer_allocations(
    partial: &executors::layerstack::SquashLayerstackPartialEvidence,
    s0: &LayerstackQuerySample,
) -> Vec<executors::layerstack::SourceLayerAllocation> {
    let observed = s0.snapshot.as_ref().ok().map(|snapshot| {
        snapshot
            .layers
            .iter()
            .map(|layer| (layer.layer_id.as_str(), layer))
            .collect::<BTreeMap<_, _>>()
    });
    partial
        .source_layer_ids
        .iter()
        .map(|layer_id| {
            let layer = observed
                .as_ref()
                .and_then(|layers| layers.get(layer_id.as_str()).copied());
            let missing_reason = || format!("source layer {layer_id} was unavailable at S0");
            executors::layerstack::SourceLayerAllocation {
                layer_id: layer_id.clone(),
                logical_bytes: layer.map_or_else(
                    || unavailable(LAYERSTACK_OBSERVATION_SOURCE, missing_reason()),
                    |layer| {
                        layer.bytes.map_or_else(
                            || {
                                unavailable(
                                    LAYERSTACK_OBSERVATION_SOURCE,
                                    format!("source layer {layer_id} logical bytes unavailable"),
                                )
                            },
                            |value| Availability::Available { value },
                        )
                    },
                ),
                allocated_bytes: layer.map_or_else(
                    || unavailable(LAYERSTACK_OBSERVATION_SOURCE, missing_reason()),
                    |layer| {
                        layer.allocated_bytes.map_or_else(
                            || {
                                unavailable(
                                    LAYERSTACK_OBSERVATION_SOURCE,
                                    format!("source layer {layer_id} allocated bytes unavailable"),
                                )
                            },
                            |value| Availability::Available { value },
                        )
                    },
                ),
            }
        })
        .collect()
}

fn reclaimed_bytes(
    partial: &executors::layerstack::SquashLayerstackPartialEvidence,
    allocations: &[executors::layerstack::SourceLayerAllocation],
) -> Availability<u64> {
    let by_id = allocations
        .iter()
        .map(|allocation| (allocation.layer_id.as_str(), &allocation.allocated_bytes))
        .collect::<BTreeMap<_, _>>();
    let sum = |ids: &[String]| -> Result<u64, String> {
        ids.iter().try_fold(0_u64, |total, id| {
            let value = by_id
                .get(id.as_str())
                .ok_or_else(|| format!("source allocation for {id} was not recorded"))?;
            let value = match value {
                Availability::Available { value } => *value,
                Availability::Unavailable { source, reason } => {
                    return Err(format!("{source}:{reason}"));
                }
            };
            total
                .checked_add(value)
                .ok_or_else(|| "source allocation sum overflowed".to_owned())
        })
    };
    let source = sum(&partial.source_layer_ids);
    let retained = sum(&partial.retained_source_layer_ids);
    match (source, retained) {
        (Ok(source), Ok(retained)) => source.checked_sub(retained).map_or_else(
            || {
                unavailable(
                    LAYERSTACK_OBSERVATION_SOURCE,
                    "retained source allocation exceeded pre-squash source allocation",
                )
            },
            |value| Availability::Available { value },
        ),
        (Err(reason), _) | (_, Err(reason)) => unavailable(
            LAYERSTACK_OBSERVATION_SOURCE,
            format!("reclaimed allocation unavailable: {reason}"),
        ),
    }
}

fn sampled_peak(
    candidates: &[executors::layerstack::StorageSnapshot],
) -> executors::layerstack::StorageSnapshot {
    let peak = candidates
        .iter()
        .filter_map(|snapshot| match &snapshot.storage_allocated_bytes {
            Availability::Available { value } => Some((*value, snapshot)),
            Availability::Unavailable { .. } => None,
        })
        .max_by_key(|(value, _)| *value)
        .map(|(_, snapshot)| snapshot.clone());
    match peak {
        Some(mut peak) => {
            peak.sampled = true;
            peak
        }
        None => unavailable_storage_snapshot(
            unavailable(
                LAYERSTACK_OBSERVATION_SOURCE,
                "sampled peak offset unavailable",
            ),
            true,
            LAYERSTACK_OBSERVATION_SOURCE,
            "no allocated LayerStack sample was available",
        ),
    }
}

fn layerstack_resource_reading(
    snapshot: &executors::layerstack::StorageSnapshot,
    source: &str,
) -> Option<ResourceReading> {
    let monotonic_offset_ns = match &snapshot.monotonic_offset_ns {
        Availability::Available { value } => *value,
        Availability::Unavailable { .. } => return None,
    };
    let value = match &snapshot.storage_allocated_bytes {
        Availability::Available { value } => Availability::Available {
            value: *value as f64,
        },
        Availability::Unavailable { source, reason } => Availability::Unavailable {
            source: source.clone(),
            reason: reason.clone(),
        },
    };
    Some(ResourceReading {
        schema_version: 1,
        metric_id: LAYERSTACK_BYTES.id.to_owned(),
        metric_semantic_revision: LAYERSTACK_BYTES.semantic_revision,
        unit: LAYERSTACK_BYTES.unit,
        scope: LAYERSTACK_BYTES.scope,
        kind: LAYERSTACK_BYTES.kind,
        aggregation: LAYERSTACK_BYTES.aggregation,
        source: source.to_owned(),
        monotonic_offset_ns,
        sampled: snapshot.sampled,
        value,
    })
}

fn trial_directory_name(run_id: &str, cell_id: &str, trial_id: &str) -> String {
    let mut digest = Sha256::new();
    for value in [run_id, cell_id, trial_id] {
        digest.update(value.len().to_le_bytes());
        digest.update(value.as_bytes());
    }
    let digest = digest.finalize();
    let mut short = String::with_capacity(TRIAL_DIRECTORY_DIGEST_BYTES * 2);
    for byte in &digest[..TRIAL_DIRECTORY_DIGEST_BYTES] {
        use std::fmt::Write as _;
        let _ = write!(&mut short, "{byte:02x}");
    }
    format!("trial-{short}")
}

fn trial_id(cell_id: &str, kind: TrialKind, sequence: u32) -> String {
    let digest = Sha256::digest(cell_id.as_bytes());
    let short = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let kind = match kind {
        TrialKind::Warmup => "warmup",
        TrialKind::Measured => "measured",
    };
    format!("trial-{short}-{kind}-{sequence:06}")
}

fn family_name(family: FamilyId) -> &'static str {
    match family {
        FamilyId::Command => "command",
        FamilyId::Files => "files",
        FamilyId::WorkspaceLifecycle => "workspace_lifecycle",
        FamilyId::LayerStack => "layerstack",
    }
}

fn expected_family(operation: OperationId) -> FamilyId {
    match operation {
        OperationId::ExecCommand => FamilyId::Command,
        OperationId::FileRead
        | OperationId::FileWrite
        | OperationId::FileEdit
        | OperationId::FileBlame => FamilyId::Files,
        OperationId::CreateWorkspace => FamilyId::WorkspaceLifecycle,
        OperationId::SquashLayerstack => FamilyId::LayerStack,
    }
}

fn selected_profile_id(operation: &ExpandedOperationCell) -> Option<&WorkspaceProfileId> {
    match operation {
        ExpandedOperationCell::ExecCommand(cell) => Some(&cell.workspace_profile),
        ExpandedOperationCell::CreateWorkspace(cell) => Some(&cell.workspace_profile),
        ExpandedOperationCell::FileRead(_)
        | ExpandedOperationCell::FileWrite(_)
        | ExpandedOperationCell::FileEdit(_)
        | ExpandedOperationCell::FileBlame(_)
        | ExpandedOperationCell::SquashLayerstack(_) => None,
    }
}

fn block_remount_parallelism(
    family: FamilyId,
    cells: &[&ExpandedCell],
) -> Result<u32, SchedulerError> {
    match family {
        FamilyId::LayerStack => {
            let mut width = None;
            for cell in cells {
                let ExpandedOperationCell::SquashLayerstack(layerstack) = &cell.operation else {
                    return Err(SchedulerError::InvalidExecutionBlock(format!(
                        "layerstack block contains non-layerstack cell {}",
                        cell.cell_id
                    )));
                };
                match width {
                    None => width = Some(layerstack.remount_parallelism),
                    Some(existing) if existing == layerstack.remount_parallelism => {}
                    Some(_) => {
                        return Err(SchedulerError::InvalidExecutionBlock(format!(
                            "layerstack block mixes remount widths at {}",
                            cell.cell_id
                        )));
                    }
                }
            }
            width.filter(|width| *width > 0).ok_or_else(|| {
                SchedulerError::InvalidExecutionBlock(
                    "layerstack block has no positive remount width".to_owned(),
                )
            })
        }
        FamilyId::Command | FamilyId::Files | FamilyId::WorkspaceLifecycle => {
            if cells
                .iter()
                .any(|cell| matches!(cell.operation, ExpandedOperationCell::SquashLayerstack(_)))
            {
                return Err(SchedulerError::InvalidExecutionBlock(
                    "non-layerstack block contains a layerstack cell".to_owned(),
                ));
            }
            Ok(1)
        }
    }
}

fn set_owner_only_directory(path: &Path) -> Result<(), SchedulerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            SchedulerError::CampaignIo {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|source| SchedulerError::CampaignIo {
                path: path.to_path_buf(),
                source,
            })?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).map_err(|source| SchedulerError::CampaignIo {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn set_owner_only_file(path: &Path) -> Result<(), SchedulerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            SchedulerError::CampaignIo {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|source| SchedulerError::CampaignIo {
                path: path.to_path_buf(),
                source,
            })?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).map_err(|source| SchedulerError::CampaignIo {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn copy_fixture_tree(source: &Path, destination: &Path) -> Result<(), SchedulerError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|source_error| SchedulerError::CampaignIo {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SchedulerError::CampaignIo {
            path: source.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture root must be a real directory",
            ),
        });
    }
    copy_fixture_directory(source, destination)?;
    File::open(destination)
        .and_then(|directory| directory.sync_all())
        .map_err(|source_error| SchedulerError::CampaignIo {
            path: destination.to_path_buf(),
            source: source_error,
        })
}

fn copy_fixture_directory(source: &Path, destination: &Path) -> Result<(), SchedulerError> {
    let mut entries = fs::read_dir(source)
        .map_err(|source_error| SchedulerError::CampaignIo {
            path: source.to_path_buf(),
            source: source_error,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source_error| SchedulerError::CampaignIo {
                    path: source.to_path_buf(),
                    source: source_error,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for source_path in entries {
        let name = source_path
            .file_name()
            .ok_or_else(|| SchedulerError::CampaignIo {
                path: source_path.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixture entry has no file name",
                ),
            })?;
        let destination_path = destination.join(name);
        let metadata = fs::symlink_metadata(&source_path).map_err(|source_error| {
            SchedulerError::CampaignIo {
                path: source_path.clone(),
                source: source_error,
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(SchedulerError::CampaignIo {
                path: source_path,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixture tree may not contain symlinks",
                ),
            });
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path).map_err(|source_error| {
                SchedulerError::CampaignIo {
                    path: destination_path.clone(),
                    source: source_error,
                }
            })?;
            set_owner_only_directory(&destination_path)?;
            copy_fixture_directory(&source_path, &destination_path)?;
            File::open(&destination_path)
                .and_then(|directory| directory.sync_all())
                .map_err(|source_error| SchedulerError::CampaignIo {
                    path: destination_path,
                    source: source_error,
                })?;
        } else if metadata.is_file() {
            let mut input =
                File::open(&source_path).map_err(|source_error| SchedulerError::CampaignIo {
                    path: source_path.clone(),
                    source: source_error,
                })?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination_path)
                .map_err(|source_error| SchedulerError::CampaignIo {
                    path: destination_path.clone(),
                    source: source_error,
                })?;
            io::copy(&mut input, &mut output).map_err(|source_error| {
                SchedulerError::CampaignIo {
                    path: destination_path.clone(),
                    source: source_error,
                }
            })?;
            output
                .sync_all()
                .map_err(|source_error| SchedulerError::CampaignIo {
                    path: destination_path.clone(),
                    source: source_error,
                })?;
            set_owner_only_file(&destination_path)?;
        } else {
            return Err(SchedulerError::CampaignIo {
                path: source_path,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixture tree may contain only regular files and directories",
                ),
            });
        }
    }
    Ok(())
}

async fn execute_campaign(
    startup: &StartupConfig,
    dependencies: &ExecutionDependencies,
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
    profiles: &MaterializedProfiles,
    clock: &Arc<MonotonicClock>,
    cancellation: &CancellationToken,
) -> Result<CampaignControl, SchedulerError> {
    let cells_by_id = expanded
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell))
        .collect::<BTreeMap<_, _>>();

    for family in FamilyId::ALL {
        let blocks = expanded
            .execution_blocks
            .iter()
            .filter(|block| block.family_id == family)
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            continue;
        }
        emit_family_state(artifacts, clock, family, WorkState::Preparing).await?;
        if cancellation.is_cancelled() {
            emit_family_state(artifacts, clock, family, WorkState::Cancelled).await?;
            return Ok(CampaignControl::Cancelled);
        }
        emit_family_state(artifacts, clock, family, WorkState::Running).await?;

        for block in blocks {
            let block_cells = block
                .cell_ids
                .iter()
                .map(|cell_id| {
                    cells_by_id.get(cell_id.as_str()).copied().ok_or_else(|| {
                        SchedulerError::InvalidExecutionBlock(format!(
                            "{} references unknown cell {cell_id}",
                            block.block_id
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let width = block_remount_parallelism(family, &block_cells)?;
            if cancellation.is_cancelled() {
                emit_family_state(artifacts, clock, family, WorkState::Cancelled).await?;
                return Ok(CampaignControl::Cancelled);
            }

            let launch = GatewayLaunchConfig::new(
                dependencies.gateway_binary.clone(),
                dependencies.daemon_binary.clone(),
                width,
            );
            let mut gateway = IsolatedGateway::start(startup, launch).await?;
            let product = gateway.product();
            let sessions = Arc::new(WorkspaceSessionAdapter::new(Arc::clone(&product)));
            let mut workspaces = BlockWorkspaceLedger::new();
            let block_result = execute_block_cells(
                startup,
                expanded,
                artifacts,
                profiles,
                clock,
                cancellation,
                width,
                &block_cells,
                &mut gateway,
                product,
                sessions,
                &mut workspaces,
            )
            .await;

            let shutdown = tokio::time::timeout(GATEWAY_STOP_TIMEOUT, gateway.stop()).await;
            let shutdown_result = match shutdown {
                Ok(Ok(shutdown)) => {
                    emit_gateway_logs(artifacts, clock, &shutdown.logs).await?;
                    workspaces.remove_all_after_gateway_stop(startup)?;
                    Ok(())
                }
                Ok(Err(error)) => Err(SchedulerError::Gateway(error)),
                Err(_) => Err(SchedulerError::GatewayStopTimeout),
            };

            match (block_result, shutdown_result) {
                (Ok(CampaignControl::Continue), Ok(())) => {}
                (Ok(CampaignControl::Cancelled), Ok(())) => {
                    emit_family_state(artifacts, clock, family, WorkState::Cancelled).await?;
                    return Ok(CampaignControl::Cancelled);
                }
                (Err(error), Ok(())) | (Ok(_), Err(error)) => {
                    emit_family_state(artifacts, clock, family, WorkState::Failed).await?;
                    return Err(error);
                }
                (Err(primary), Err(shutdown)) => {
                    emit_warning(
                        artifacts,
                        clock,
                        "gateway_shutdown_after_failure",
                        &shutdown.to_string(),
                    )
                    .await?;
                    emit_family_state(artifacts, clock, family, WorkState::Failed).await?;
                    return Err(primary);
                }
            }
        }
        emit_family_state(artifacts, clock, family, WorkState::Completed).await?;
    }
    Ok(CampaignControl::Continue)
}

#[allow(clippy::too_many_arguments)]
async fn execute_block_cells(
    startup: &StartupConfig,
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
    profiles: &MaterializedProfiles,
    clock: &Arc<MonotonicClock>,
    cancellation: &CancellationToken,
    remount_parallelism: u32,
    cells: &[&ExpandedCell],
    gateway: &mut IsolatedGateway,
    product: Arc<ProductGateway>,
    sessions: Arc<WorkspaceSessionAdapter>,
    workspaces: &mut BlockWorkspaceLedger,
) -> Result<CampaignControl, SchedulerError> {
    let interval =
        SamplingInterval::from_millis(expanded.canonical_plan.protocol.resource_interval_ms)
            .map_err(|_| {
                SchedulerError::InvalidResourceInterval(
                    expanded.canonical_plan.protocol.resource_interval_ms,
                )
            })?;

    for cell in cells {
        gateway.ensure_alive()?;
        emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Preparing).await?;
        if cancellation.is_cancelled() {
            emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Cancelled).await?;
            return Ok(CampaignControl::Cancelled);
        }
        let prepared_cell = if requires_prepared_cell_sandbox(cell.operation.resolved_isolation()) {
            match prepare_cell_sandbox(
                startup,
                expanded,
                artifacts,
                profiles,
                cell,
                clock,
                cancellation,
                &product,
                workspaces,
            )
            .await
            {
                Ok(CellSandboxPreparation::Ready(prepared)) => Some(prepared),
                Ok(CellSandboxPreparation::Cancelled) => {
                    emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Cancelled).await?;
                    return Ok(CampaignControl::Cancelled);
                }
                Err(error) => {
                    emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Failed).await?;
                    return Err(error);
                }
            }
        } else {
            None
        };
        emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Running).await?;

        let mut cell_control = CampaignControl::Continue;
        let mut cell_error = None;
        'trials: for (kind, count) in [
            (TrialKind::Warmup, cell.protocol.warmups),
            (TrialKind::Measured, cell.protocol.measured_trials),
        ] {
            for sequence in 0..count {
                if cancellation.is_cancelled() {
                    cell_control = CampaignControl::Cancelled;
                    break 'trials;
                }
                if let Err(error) = gateway.ensure_alive() {
                    cell_error = Some(error.into());
                    break 'trials;
                }
                let result = match execute_trial(
                    startup,
                    expanded,
                    artifacts,
                    profiles,
                    clock,
                    cancellation,
                    remount_parallelism,
                    interval,
                    cell,
                    kind,
                    sequence,
                    Arc::clone(&product),
                    Arc::clone(&sessions),
                    workspaces,
                    prepared_cell.as_ref(),
                )
                .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        cell_error = Some(error);
                        break 'trials;
                    }
                };
                if let Some(error) = result.abort {
                    cell_error = Some(error);
                    break 'trials;
                }
                if result.control == CampaignControl::Cancelled {
                    cell_control = CampaignControl::Cancelled;
                    break 'trials;
                }
            }
        }

        let cell_cleanup = if let Some(prepared) = &prepared_cell {
            destroy_trial_sandbox(
                startup,
                artifacts,
                cell,
                PREPARED_CELL_LIFECYCLE_ID,
                &product,
                &prepared.sandbox_id,
                &prepared.workspace_root,
                &prepared.workspace_identity,
                workspaces,
            )
            .await
        } else {
            Ok(())
        };

        match (cell_error, cell_cleanup) {
            (Some(primary), Err(cleanup)) => {
                emit_warning(
                    artifacts,
                    clock,
                    "prepared_cell_cleanup_after_failure",
                    &cleanup.to_string(),
                )
                .await?;
                emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Failed).await?;
                return Err(primary);
            }
            (Some(error), Ok(())) | (None, Err(error)) => {
                emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Failed).await?;
                return Err(error);
            }
            (None, Ok(())) => {}
        }
        if cell_control == CampaignControl::Cancelled {
            emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Cancelled).await?;
            return Ok(CampaignControl::Cancelled);
        }
        emit_cell_state(artifacts, clock, &cell.cell_id, WorkState::Completed).await?;
    }
    Ok(CampaignControl::Continue)
}

#[allow(clippy::too_many_arguments)]
async fn execute_trial(
    startup: &StartupConfig,
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
    profiles: &MaterializedProfiles,
    clock: &Arc<MonotonicClock>,
    cancellation: &CancellationToken,
    remount_parallelism: u32,
    interval: SamplingInterval,
    cell: &ExpandedCell,
    kind: TrialKind,
    sequence: u32,
    product: Arc<ProductGateway>,
    sessions: Arc<WorkspaceSessionAdapter>,
    workspaces: &mut BlockWorkspaceLedger,
    prepared_cell: Option<&PreparedCellSandbox>,
) -> Result<TrialExecutionResult, SchedulerError> {
    let trial_id = trial_id(&cell.cell_id, kind, sequence);
    let warmup = kind == TrialKind::Warmup;
    emit_trial_state(
        artifacts,
        clock,
        &cell.cell_id,
        &trial_id,
        warmup,
        WorkState::Preparing,
    )
    .await?;
    emit_trial_phase(
        artifacts,
        clock,
        &cell.cell_id,
        &trial_id,
        warmup,
        LifecyclePhase::Setup,
        WorkState::Running,
    )
    .await?;

    let setup_started = Instant::now();
    let owns_sandbox = prepared_cell.is_none();
    let (workspace_root, workspace_identity) = match prepared_cell {
        Some(prepared) => (
            prepared.workspace_root.clone(),
            prepared.workspace_identity.clone(),
        ),
        None => {
            workspaces.create_trial_root(startup, &artifacts.run_id, &cell.cell_id, &trial_id)?
        }
    };

    if owns_sandbox {
        if let Some(profile_id) = selected_profile_id(&cell.operation) {
            let fixture = profiles.by_id.get(profile_id).ok_or_else(|| {
                SchedulerError::InvalidPlan(format!(
                    "cell {} selected unmaterialized workspace profile {profile_id}",
                    cell.cell_id
                ))
            });
            let fixture = match fixture {
                Ok(fixture) => fixture,
                Err(error) => {
                    workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
                    return record_setup_failure(
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        true,
                        error,
                    )
                    .await;
                }
            };
            let source = fixture.path.clone();
            let destination = workspace_root.clone();
            let copied =
                tokio::task::spawn_blocking(move || copy_fixture_tree(&source, &destination))
                    .await
                    .map_err(|error| SchedulerError::CampaignTask(error.to_string()))
                    .and_then(std::convert::identity);
            if let Err(error) = copied {
                workspaces.remove_trial_root(startup, &workspace_root, &workspace_identity)?;
                return record_setup_failure(
                    artifacts,
                    clock,
                    cell,
                    &trial_id,
                    kind,
                    sequence,
                    setup_started,
                    true,
                    error,
                )
                .await;
            }
        }
    }

    if cancellation.is_cancelled() {
        let cleanup = if owns_sandbox {
            SetupCancellationCleanup::RootOnly
        } else {
            SetupCancellationCleanup::DeferredToGatewayOrCellOwner
        };
        return finish_cancelled_setup(
            startup,
            artifacts,
            clock,
            cell,
            &trial_id,
            kind,
            sequence,
            setup_started,
            &product,
            &workspace_root,
            &workspace_identity,
            workspaces,
            cleanup,
        )
        .await;
    }

    let sandbox_id = match prepared_cell {
        Some(prepared) => prepared.sandbox_id.clone(),
        None => {
            let create_correlation = Correlation::new(
                &artifacts.run_id,
                &cell.cell_id,
                &trial_id,
                "create-sandbox",
            )?;
            let image = expanded.canonical_plan.environment.image.0.clone();
            let created = await_owned_task(
                cancellation,
                Some(SANDBOX_CREATE_TIMEOUT),
                CancellationBoundary::SandboxCreate,
                product.create_sandbox(&image, &workspace_root, &create_correlation),
            )
            .await;
            match created {
                OwnedTaskOutcome::Completed(Ok(sandbox_id)) => sandbox_id,
                OwnedTaskOutcome::Completed(Err(error)) => {
                    // A transport failure can occur after product-side creation. Keep
                    // the marker-owned root until isolated gateway shutdown has made
                    // product access impossible, then the block ledger removes it.
                    return record_setup_failure(
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        false,
                        SchedulerError::Gateway(error),
                    )
                    .await;
                }
                OwnedTaskOutcome::TimedOut => {
                    return record_setup_failure(
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        false,
                        SchedulerError::SandboxCreateTimeout(trial_id.clone()),
                    )
                    .await;
                }
                OwnedTaskOutcome::CancelledBeforeStart => {
                    return finish_cancelled_setup(
                        startup,
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        &product,
                        &workspace_root,
                        &workspace_identity,
                        workspaces,
                        SetupCancellationCleanup::RootOnly,
                    )
                    .await;
                }
                OwnedTaskOutcome::CancelledCompleted(Ok(sandbox_id)) => {
                    return finish_cancelled_setup(
                        startup,
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        &product,
                        &workspace_root,
                        &workspace_identity,
                        workspaces,
                        SetupCancellationCleanup::OwnedSandbox(sandbox_id),
                    )
                    .await;
                }
                OwnedTaskOutcome::CancelledCompleted(Err(error)) => {
                    emit_warning(
                        artifacts,
                        clock,
                        "cancelled_trial_create_ambiguous",
                        &error.to_string(),
                    )
                    .await?;
                    return finish_cancelled_setup(
                        startup,
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        &product,
                        &workspace_root,
                        &workspace_identity,
                        workspaces,
                        SetupCancellationCleanup::DeferredToGatewayOrCellOwner,
                    )
                    .await;
                }
                OwnedTaskOutcome::CancelledAfterGrace => {
                    emit_warning(
                        artifacts,
                        clock,
                        "trial_create_cancellation_grace_expired",
                        "sandbox creation did not settle within its cancellation grace; gateway shutdown owns cleanup",
                    )
                    .await?;
                    return finish_cancelled_setup(
                        startup,
                        artifacts,
                        clock,
                        cell,
                        &trial_id,
                        kind,
                        sequence,
                        setup_started,
                        &product,
                        &workspace_root,
                        &workspace_identity,
                        workspaces,
                        SetupCancellationCleanup::DeferredToGatewayOrCellOwner,
                    )
                    .await;
                }
            }
        }
    };

    if cancellation.is_cancelled() {
        let cleanup = if owns_sandbox {
            SetupCancellationCleanup::OwnedSandbox(sandbox_id.clone())
        } else {
            SetupCancellationCleanup::DeferredToGatewayOrCellOwner
        };
        return finish_cancelled_setup(
            startup,
            artifacts,
            clock,
            cell,
            &trial_id,
            kind,
            sequence,
            setup_started,
            &product,
            &workspace_root,
            &workspace_identity,
            workspaces,
            cleanup,
        )
        .await;
    }

    let context = match RuntimeContext::new(
        Arc::clone(&product),
        sessions,
        sandbox_id.clone(),
        &artifacts.run_id,
        &cell.cell_id,
        &trial_id,
        cell.protocol.timeout_ms,
        remount_parallelism,
    ) {
        Ok(context) => context,
        Err(error) => {
            let cleanup = if owns_sandbox {
                destroy_trial_sandbox(
                    startup,
                    artifacts,
                    cell,
                    &trial_id,
                    &product,
                    &sandbox_id,
                    &workspace_root,
                    &workspace_identity,
                    workspaces,
                )
                .await
            } else {
                Ok(())
            };
            cleanup?;
            return record_setup_failure(
                artifacts,
                clock,
                cell,
                &trial_id,
                kind,
                sequence,
                setup_started,
                owns_sandbox,
                SchedulerError::Executor(error),
            )
            .await;
        }
    };

    if cancellation.is_cancelled() {
        let cleanup = if owns_sandbox {
            SetupCancellationCleanup::OwnedSandbox(sandbox_id.clone())
        } else {
            SetupCancellationCleanup::DeferredToGatewayOrCellOwner
        };
        return finish_cancelled_setup(
            startup,
            artifacts,
            clock,
            cell,
            &trial_id,
            kind,
            sequence,
            setup_started,
            &product,
            &workspace_root,
            &workspace_identity,
            workspaces,
            cleanup,
        )
        .await;
    }

    let preparation = await_owned_task(
        cancellation,
        None,
        CancellationBoundary::OperationPrepare(cell.operation_id),
        StaticLifecycleDriver::<ClosedOperationLifecycle>::prepare(&context, &cell.operation),
    )
    .await;
    let mut prepared = match preparation {
        OwnedTaskOutcome::Completed(Ok(prepared))
        | OwnedTaskOutcome::CancelledCompleted(Ok(prepared)) => prepared,
        OwnedTaskOutcome::Completed(Err(error)) => {
            let cleanup = if owns_sandbox {
                destroy_trial_sandbox(
                    startup,
                    artifacts,
                    cell,
                    &trial_id,
                    &product,
                    &sandbox_id,
                    &workspace_root,
                    &workspace_identity,
                    workspaces,
                )
                .await
            } else {
                Ok(())
            };
            cleanup?;
            return record_setup_failure(
                artifacts,
                clock,
                cell,
                &trial_id,
                kind,
                sequence,
                setup_started,
                owns_sandbox,
                SchedulerError::Executor(error),
            )
            .await;
        }
        OwnedTaskOutcome::TimedOut => {
            return Err(SchedulerError::CampaignTask(
                "operation preparation unexpectedly timed out without a deadline".to_owned(),
            ));
        }
        OwnedTaskOutcome::CancelledBeforeStart => {
            let cleanup = if owns_sandbox {
                SetupCancellationCleanup::OwnedSandbox(sandbox_id.clone())
            } else {
                SetupCancellationCleanup::DeferredToGatewayOrCellOwner
            };
            return finish_cancelled_setup(
                startup,
                artifacts,
                clock,
                cell,
                &trial_id,
                kind,
                sequence,
                setup_started,
                &product,
                &workspace_root,
                &workspace_identity,
                workspaces,
                cleanup,
            )
            .await;
        }
        OwnedTaskOutcome::CancelledCompleted(Err(error)) => {
            emit_warning(
                artifacts,
                clock,
                "cancelled_operation_prepare_failed",
                &error.to_string(),
            )
            .await?;
            let cleanup = if owns_sandbox {
                SetupCancellationCleanup::OwnedSandbox(sandbox_id.clone())
            } else {
                SetupCancellationCleanup::DeferredToGatewayOrCellOwner
            };
            return finish_cancelled_setup(
                startup,
                artifacts,
                clock,
                cell,
                &trial_id,
                kind,
                sequence,
                setup_started,
                &product,
                &workspace_root,
                &workspace_identity,
                workspaces,
                cleanup,
            )
            .await;
        }
        OwnedTaskOutcome::CancelledAfterGrace => {
            emit_warning(
                artifacts,
                clock,
                "operation_prepare_cancellation_grace_expired",
                "operation preparation did not settle within its cancellation grace; sandbox teardown owns cleanup",
            )
            .await?;
            let cleanup = if owns_sandbox {
                SetupCancellationCleanup::OwnedSandbox(sandbox_id.clone())
            } else {
                SetupCancellationCleanup::DeferredToGatewayOrCellOwner
            };
            return finish_cancelled_setup(
                startup,
                artifacts,
                clock,
                cell,
                &trial_id,
                kind,
                sequence,
                setup_started,
                &product,
                &workspace_root,
                &workspace_identity,
                workspaces,
                cleanup,
            )
            .await;
        }
    };

    // From this point on no artifact or executor failure may return before the
    // prepared operation is torn down. Trial-owned sandboxes are destroyed
    // here; prepared-per-cell sandboxes are destroyed by the cell owner.
    let mut deferred_error = None;
    let expected_invocations = cell.operation.measured_invocation_count();
    let invocations = match StaticLifecycleDriver::<ClosedOperationLifecycle>::validated_invocations(
        &prepared,
        &cell.operation,
        expected_invocations,
    ) {
        Ok(invocations) => invocations,
        Err(StaticInvocationBuildError::Generation(error)) => {
            deferred_error = Some(SchedulerError::Executor(error));
            Vec::new()
        }
        Err(StaticInvocationBuildError::Count(mismatch)) => {
            deferred_error = Some(SchedulerError::InvocationCountMismatch {
                operation: cell.operation_id,
                expected: mismatch.expected,
                actual: mismatch.actual,
            });
            Vec::new()
        }
    };

    let mut layerstack_warnings = Vec::new();
    let layerstack_s0 = if cell.operation_id == OperationId::SquashLayerstack
        && !cancellation.is_cancelled()
    {
        let sample = observe_layerstack_query(
            &product,
            &sandbox_id,
            &artifacts.run_id,
            &cell.cell_id,
            &trial_id,
            "layerstack-s0",
            false,
            clock,
        )
        .await;
        if let Err(reason) = &sample.snapshot {
            layerstack_warnings.push(format!("S0 LayerStack observation unavailable: {reason}"));
        }
        Some(sample)
    } else {
        None
    };
    let mut layerstack_operation_samples = Vec::new();
    let mut phase_observations = Vec::new();
    let mut specialized_evidence = None;
    let mut layerstack_settled_staging_absent = None;

    let mut sampled_resources =
        match collect_resource_snapshot(clock, &workspace_root, &startup.paths.root).await {
            Ok(readings) => readings,
            Err(error) => {
                retain_scheduler_error(&mut deferred_error, Err(error));
                Vec::new()
            }
        };
    if !cancellation.is_cancelled() {
        sampled_resources.extend(product_resource_readings(
            observe_product_resources(
                &product,
                &sandbox_id,
                &artifacts.run_id,
                &cell.cell_id,
                &trial_id,
                "resources-baseline",
                false,
                clock,
            )
            .await,
        ));
    }

    let setup_ns = elapsed_ns(setup_started);
    let setup_state = if cancellation.is_cancelled() {
        WorkState::Cancelled
    } else if deferred_error.is_some() {
        WorkState::Failed
    } else {
        WorkState::Completed
    };
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            LifecyclePhase::Setup,
            setup_state,
        )
        .await,
    );

    let operation_started = Instant::now();
    let mut timed_outcomes = Vec::new();
    let operation_status;
    if deferred_error.is_none() && !cancellation.is_cancelled() {
        retain_scheduler_error(
            &mut deferred_error,
            emit_trial_phase(
                artifacts,
                clock,
                &cell.cell_id,
                &trial_id,
                warmup,
                LifecyclePhase::Operation,
                WorkState::Running,
            )
            .await,
        );
        for invocation in &invocations {
            retain_scheduler_error(
                &mut deferred_error,
                emit_request_state(
                    artifacts,
                    clock,
                    &cell.cell_id,
                    &trial_id,
                    invocation.request_id(),
                    RequestState::WaitingAtBarrier,
                )
                .await,
            );
        }
        let sampler = ResourceSampler::start(interval, Arc::clone(clock));
        let product_resource_sampler = ProductResourceSampler::start(
            interval,
            Arc::clone(clock),
            Arc::clone(&product),
            sandbox_id.clone(),
            artifacts.run_id.clone(),
            cell.cell_id.clone(),
            trial_id.clone(),
        );
        let layerstack_sampler = (cell.operation_id == OperationId::SquashLayerstack).then(|| {
            LayerstackSampler::start(
                interval,
                Arc::clone(clock),
                Arc::clone(&product),
                sandbox_id.clone(),
                artifacts.run_id.clone(),
                cell.cell_id.clone(),
                trial_id.clone(),
            )
        });
        timed_outcomes = invoke_batch_at_barrier(
            artifacts,
            clock,
            cancellation,
            &context,
            cell.operation_id,
            cell.protocol.timeout_ms,
            invocations,
            &mut deferred_error,
        )
        .await;
        match sampler.finish().await {
            Ok(readings) => sampled_resources.extend(readings),
            Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
        }
        match product_resource_sampler.finish().await {
            Ok(samples) => {
                sampled_resources.extend(samples.into_iter().flat_map(product_resource_readings))
            }
            Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
        }
        if let Some(sampler) = layerstack_sampler {
            match sampler.finish().await {
                Ok(samples) => {
                    for sample in &samples {
                        if let Err(reason) = &sample.snapshot {
                            layerstack_warnings.push(format!(
                                "sampled LayerStack observation unavailable: {reason}"
                            ));
                        }
                    }
                    layerstack_operation_samples.extend(samples);
                }
                Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
            }
        }
        operation_status = if cancellation.is_cancelled() {
            PhaseStatus::Cancelled
        } else if timed_outcomes.iter().any(|outcome| {
            matches!(
                outcome.outcome.error(),
                Some(ExecutorError::RequestTimedOut { .. })
            )
        }) {
            PhaseStatus::TimedOut
        } else if timed_outcomes.iter().all(|outcome| {
            outcome
                .outcome
                .response_metadata()
                .is_some_and(|metadata| metadata.status == ProductOutputStatus::Succeeded)
        }) {
            PhaseStatus::Succeeded
        } else {
            PhaseStatus::Failed
        };
    } else {
        operation_status = if cancellation.is_cancelled() {
            PhaseStatus::Cancelled
        } else {
            PhaseStatus::Failed
        };
    }
    let operation_ns = elapsed_ns(operation_started);
    match collect_resource_snapshot(clock, &workspace_root, &startup.paths.root).await {
        Ok(readings) => sampled_resources.extend(readings),
        Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
    }
    if !cancellation.is_cancelled() {
        sampled_resources.extend(product_resource_readings(
            observe_product_resources(
                &product,
                &sandbox_id,
                &artifacts.run_id,
                &cell.cell_id,
                &trial_id,
                "resources-post-operation",
                false,
                clock,
            )
            .await,
        ));
    }
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            LifecyclePhase::Operation,
            work_state_for_phase(operation_status),
        )
        .await,
    );

    let product_succeeded = timed_outcomes.len()
        == usize::try_from(expected_invocations).unwrap_or(usize::MAX)
        && timed_outcomes.iter().all(|outcome| {
            outcome
                .outcome
                .response_metadata()
                .is_some_and(|metadata| metadata.status == ProductOutputStatus::Succeeded)
        });
    let request_records = timed_request_observations(&timed_outcomes, &cell.cell_id, &trial_id);

    let verify_started = Instant::now();
    let mut checks = Vec::new();
    let mut verification_failed = false;
    let verification_started = !cancellation.is_cancelled();
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            LifecyclePhase::Verify,
            if verification_started {
                WorkState::Running
            } else {
                WorkState::Skipped
            },
        )
        .await,
    );
    let operation_outcomes = timed_outcomes
        .into_iter()
        .map(|timed| timed.outcome)
        .collect::<Vec<_>>();
    if verification_started && !operation_outcomes.is_empty() {
        match await_owned_task(
            cancellation,
            None,
            CancellationBoundary::OperationVerify(cell.operation_id),
            StaticLifecycleDriver::<ClosedOperationLifecycle>::verify(
                &context,
                &prepared,
                &cell.operation,
                &operation_outcomes,
            ),
        )
        .await
        {
            OwnedTaskOutcome::Completed(Ok(Verification {
                checks: verification_checks,
            }))
            | OwnedTaskOutcome::CancelledCompleted(Ok(Verification {
                checks: verification_checks,
            })) => checks.extend(verification_checks),
            OwnedTaskOutcome::Completed(Err(error))
            | OwnedTaskOutcome::CancelledCompleted(Err(error)) => {
                verification_failed = true;
                retain_scheduler_error(&mut deferred_error, Err(SchedulerError::Executor(error)));
            }
            OwnedTaskOutcome::CancelledBeforeStart => {
                verification_failed = true;
            }
            OwnedTaskOutcome::CancelledAfterGrace => {
                verification_failed = true;
                retain_scheduler_error(
                    &mut deferred_error,
                    emit_warning(
                        artifacts,
                        clock,
                        "operation_verify_cancellation_grace_expired",
                        "operation verification did not settle within its cancellation grace; teardown owns cleanup",
                    )
                    .await,
                );
            }
            OwnedTaskOutcome::TimedOut => {
                verification_failed = true;
                retain_scheduler_error(
                    &mut deferred_error,
                    Err(SchedulerError::CampaignTask(
                        "operation verification unexpectedly timed out without a deadline"
                            .to_owned(),
                    )),
                );
            }
        }
    } else {
        verification_failed = true;
    }
    let verify_ns = elapsed_ns(verify_started);
    if verification_started {
        retain_scheduler_error(
            &mut deferred_error,
            emit_trial_phase(
                artifacts,
                clock,
                &cell.cell_id,
                &trial_id,
                warmup,
                LifecyclePhase::Verify,
                if cancellation.is_cancelled() {
                    WorkState::Cancelled
                } else if verification_failed {
                    WorkState::Failed
                } else {
                    WorkState::Completed
                },
            )
            .await,
        );
    }

    if cell.operation_id == OperationId::SquashLayerstack && !cancellation.is_cancelled() {
        let expected_live_sessions = match &cell.operation {
            ExpandedOperationCell::SquashLayerstack(layerstack) => layerstack.live_sessions,
            ExpandedOperationCell::ExecCommand(_)
            | ExpandedOperationCell::FileRead(_)
            | ExpandedOperationCell::FileWrite(_)
            | ExpandedOperationCell::FileEdit(_)
            | ExpandedOperationCell::FileBlame(_)
            | ExpandedOperationCell::CreateWorkspace(_) => 0,
        };
        let request_start_offset_ns = request_records
            .iter()
            .find(|request| request.request_id == LAYERSTACK_REQUEST_ID)
            .map(|request| request.start_offset_ns);
        let trace = if request_start_offset_ns.is_some() {
            observe_layerstack_trace(
                &product,
                &sandbox_id,
                &artifacts.run_id,
                &cell.cell_id,
                &trial_id,
            )
            .await
        } else {
            Err("the squash request was not issued".to_owned())
        };
        let trace_collection = collect_layerstack_trace(
            trace.as_ref().map_err(String::as_str),
            &cell.cell_id,
            &trial_id,
            request_start_offset_ns,
            expected_live_sessions,
        );
        phase_observations.extend(trace_collection.phases);
        layerstack_warnings.extend(trace_collection.warnings);

        let (s3, _settle_samples, settle_warning) = settle_layerstack(
            &product,
            &sandbox_id,
            &artifacts.run_id,
            &cell.cell_id,
            &trial_id,
            clock,
        )
        .await;
        if let Some(warning) = settle_warning {
            layerstack_warnings.push(warning);
        }
        layerstack_settled_staging_absent = Some(
            s3.snapshot
                .as_ref()
                .ok()
                .and_then(|snapshot| snapshot.staging_entry_count)
                == Some(0),
        );
        match (
            prepared.squash_layerstack_partial_evidence(),
            layerstack_s0.as_ref(),
        ) {
            (Ok(partial), Some(s0)) => {
                for sample in &layerstack_operation_samples {
                    let snapshot = storage_snapshot(sample);
                    if let Some(reading) =
                        layerstack_resource_reading(&snapshot, LAYERSTACK_OBSERVATION_SOURCE)
                    {
                        sampled_resources.push(reading);
                    }
                }
                let collected = collect_layerstack_evidence(
                    &partial,
                    remount_parallelism,
                    s0,
                    &layerstack_operation_samples,
                    trace_collection.s2_post_commit,
                    &s3,
                );
                for (snapshot, source) in [
                    (&collected.s0_baseline, LAYERSTACK_OBSERVATION_SOURCE),
                    (&collected.s1_sampled_peak, LAYERSTACK_OBSERVATION_SOURCE),
                    (&collected.s2_post_commit, LAYERSTACK_TRACE_SOURCE),
                    (&collected.s3_settled, LAYERSTACK_OBSERVATION_SOURCE),
                ] {
                    if let Some(reading) = layerstack_resource_reading(snapshot, source) {
                        sampled_resources.push(reading);
                    }
                }
                specialized_evidence = Some(partial.finalize(collected));
            }
            (Err(error), _) if !product_succeeded => layerstack_warnings.push(format!(
                "LayerStack typed evidence unavailable after product failure: {error}"
            )),
            (Err(error), _) => {
                retain_scheduler_error(&mut deferred_error, Err(SchedulerError::Executor(error)))
            }
            (Ok(_), None) => retain_scheduler_error(
                &mut deferred_error,
                Err(SchedulerError::ResourceTask(
                    "LayerStack S0 collector was not started".to_owned(),
                )),
            ),
        }
        for warning in &layerstack_warnings {
            retain_scheduler_error(
                &mut deferred_error,
                emit_warning(artifacts, clock, "layerstack_evidence_warning", warning).await,
            );
        }
    }

    let teardown_started = Instant::now();
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            LifecyclePhase::Teardown,
            WorkState::Running,
        )
        .await,
    );
    let teardown = match tokio::time::timeout(
        OPERATION_TEARDOWN_TIMEOUT,
        StaticLifecycleDriver::<ClosedOperationLifecycle>::teardown(&context, &mut prepared),
    )
    .await
    {
        Ok(result) => Some(result),
        Err(_) => {
            retain_scheduler_error(
                &mut deferred_error,
                Err(SchedulerError::OperationTeardownTimeout(trial_id.clone())),
            );
            None
        }
    };
    if let Some(teardown) = &teardown {
        checks.extend(teardown.checks.clone());
        if !teardown.errors.is_empty() || !teardown.baseline_restored {
            retain_scheduler_error(
                &mut deferred_error,
                Err(SchedulerError::CleanupBaseline {
                    trial_id: trial_id.clone(),
                    detail: bounded_event_text(&format!(
                        "operation teardown errors={}, baseline_restored={}",
                        teardown.errors.len(),
                        teardown.baseline_restored
                    )),
                }),
            );
        }
    }

    let destroyed = if owns_sandbox {
        destroy_trial_sandbox(
            startup,
            artifacts,
            cell,
            &trial_id,
            &product,
            &sandbox_id,
            &workspace_root,
            &workspace_identity,
            workspaces,
        )
        .await
    } else {
        Ok(())
    };
    let sandbox_destroyed = owns_sandbox && destroyed.is_ok();
    let sandbox_cleanup_succeeded = destroyed.is_ok();
    let cleanup_succeeded = teardown
        .as_ref()
        .is_some_and(|teardown| teardown.baseline_restored && teardown.errors.is_empty())
        && sandbox_cleanup_succeeded;
    if let Err(error) = destroyed {
        retain_scheduler_error(&mut deferred_error, Err(error));
    }

    if cell.operation_id == OperationId::SquashLayerstack && !cancellation.is_cancelled() {
        let started = Instant::now();
        let root_absent = fs::symlink_metadata(&workspace_root)
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        let settled_staging_absent = layerstack_settled_staging_absent == Some(true);
        checks.push(executors::check_result(
            &context,
            OperationId::SquashLayerstack,
            CheckId::LayerstackResidue,
            None,
            cleanup_succeeded && root_absent && settled_staging_absent,
            "settled_staging_absent=true,sandbox_destroyed=true,trial_root_absent=true",
            format!(
                "settled_staging_absent={settled_staging_absent},sandbox_destroyed={},trial_root_absent={root_absent}",
                sandbox_destroyed,
            ),
            started,
        ));
    }

    let mut evidence = None;
    match cell.operation_id {
        OperationId::SquashLayerstack => evidence = specialized_evidence,
        OperationId::ExecCommand
        | OperationId::FileRead
        | OperationId::FileWrite
        | OperationId::FileEdit
        | OperationId::FileBlame
        | OperationId::CreateWorkspace => {
            if let Some(teardown) = &teardown {
                match executors::operation_evidence(
                    &prepared,
                    &cell.operation,
                    &operation_outcomes,
                    teardown,
                ) {
                    Ok(observed) => evidence = Some(observed),
                    Err(error) if !product_succeeded => {
                        retain_scheduler_error(
                            &mut deferred_error,
                            emit_warning(
                                artifacts,
                                clock,
                                "evidence_unavailable_after_product_failure",
                                &error.to_string(),
                            )
                            .await,
                        );
                    }
                    Err(error) => retain_scheduler_error(
                        &mut deferred_error,
                        Err(SchedulerError::Executor(error)),
                    ),
                }
            }
        }
    }

    let teardown_ns = elapsed_ns(teardown_started);
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            LifecyclePhase::Teardown,
            if cleanup_succeeded {
                WorkState::Completed
            } else {
                WorkState::Failed
            },
        )
        .await,
    );

    let snapshot_resources = collect_resource_snapshot(clock, &workspace_root, &startup.paths.root)
        .await
        .unwrap_or_else(|error| {
            retain_scheduler_error(&mut deferred_error, Err(error));
            Vec::new()
        });
    sampled_resources.extend(snapshot_resources);
    if cell.operation_id == OperationId::SquashLayerstack {
        sampled_resources.retain(|reading| {
            reading.metric_id != LAYERSTACK_BYTES.id || reading.source != "scheduler_discovery"
        });
    }
    for reading in sampled_resources {
        retain_scheduler_error(
            &mut deferred_error,
            persist_resource(artifacts, clock, cell, &trial_id, reading).await,
        );
    }

    for request in request_records {
        retain_scheduler_error(
            &mut deferred_error,
            artifacts
                .append_observation(ObservationRecord::Request(request))
                .await,
        );
    }
    for phase in phase_observations {
        retain_scheduler_error(
            &mut deferred_error,
            artifacts
                .append_observation(ObservationRecord::Phase(phase))
                .await,
        );
    }
    for check in &checks {
        retain_scheduler_error(
            &mut deferred_error,
            persist_check(artifacts, clock, check).await,
        );
    }
    let mut evidence_artifacts = Vec::new();
    if let Some(evidence) = evidence {
        match artifacts
            .write_trial_evidence(&cell.cell_id, &trial_id, evidence.clone())
            .await
        {
            Ok(reference) => evidence_artifacts.push(reference),
            Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
        }
        let request_id = match cell.operation_id {
            OperationId::SquashLayerstack => Some("squash-layerstack-0".to_owned()),
            OperationId::ExecCommand
            | OperationId::FileRead
            | OperationId::FileWrite
            | OperationId::FileEdit
            | OperationId::FileBlame
            | OperationId::CreateWorkspace => None,
        };
        retain_scheduler_error(
            &mut deferred_error,
            artifacts
                .append_observation(ObservationRecord::Operation(OperationObservation {
                    operation_id: cell.operation_id,
                    cell_id: cell.cell_id.clone(),
                    trial_id: trial_id.clone(),
                    request_id,
                    evidence,
                }))
                .await,
        );
    }

    let correctness = fold_correctness(
        cell.operation_id,
        product_succeeded,
        cleanup_succeeded,
        &checks,
    );
    let infrastructure_failed = deferred_error.is_some();
    retain_scheduler_error(
        &mut deferred_error,
        artifacts
            .append_observation(ObservationRecord::Trial(TrialSample {
                operation_id: cell.operation_id,
                cell_id: cell.cell_id.clone(),
                trial_id: trial_id.clone(),
                kind,
                sequence_in_cell: sequence,
                lifecycle: LifecycleDurations {
                    setup_ns,
                    operation_ns,
                    verify_ns,
                    teardown_ns,
                },
                product_succeeded,
                infrastructure_failed,
                cleanup_baseline_restored: cleanup_succeeded,
                correctness,
                primary_operation_latency_ns: (!operation_outcomes.is_empty())
                    .then_some(operation_ns),
                artifacts: evidence_artifacts,
            }))
            .await,
    );

    let correctness_failed = checks
        .iter()
        .any(|check| check.verdict == CheckVerdict::Fail);
    let final_state = if cancellation.is_cancelled() {
        WorkState::Cancelled
    } else if deferred_error.is_some() || !product_succeeded || correctness_failed {
        WorkState::Failed
    } else {
        WorkState::Completed
    };
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_state(
            artifacts,
            clock,
            &cell.cell_id,
            &trial_id,
            warmup,
            final_state,
        )
        .await,
    );

    if cancellation.is_cancelled() {
        return Ok(TrialExecutionResult::cancelled());
    }
    Ok(deferred_error.map_or_else(
        TrialExecutionResult::continue_campaign,
        TrialExecutionResult::abort,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn invoke_batch_at_barrier(
    artifacts: &RunArtifacts,
    clock: &Arc<MonotonicClock>,
    cancellation: &CancellationToken,
    context: &RuntimeContext,
    operation: OperationId,
    timeout_ms: u64,
    invocations: Vec<crate::executors::OperationInvocation>,
    deferred_error: &mut Option<SchedulerError>,
) -> Vec<TimedOperationOutcome> {
    let timed = StaticLifecycleDriver::<ClosedOperationLifecycle>::invoke_batch_with(
        invocations,
        || clock.offset_ns(),
        |invocation| async move {
            let request_id = invocation.request_id().to_owned();
            let (outcome, event_error) = if cancellation.is_cancelled() {
                (
                    OperationOutcome::failed(
                        operation,
                        &request_id,
                        ExecutorError::RequestCancelled { operation },
                    ),
                    None,
                )
            } else {
                let event_error = emit_request_state(
                    artifacts,
                    clock,
                    context.cell_id(),
                    context.trial_id(),
                    &request_id,
                    RequestState::InFlight,
                )
                .await
                .err();
                let outcome = match await_owned_task(
                    cancellation,
                    Some(Duration::from_millis(timeout_ms)),
                    CancellationBoundary::OperationRequest(operation),
                    StaticLifecycleDriver::<ClosedOperationLifecycle>::invoke_one(
                        context, invocation,
                    ),
                )
                .await
                {
                    OwnedTaskOutcome::Completed(outcome)
                    | OwnedTaskOutcome::CancelledCompleted(outcome) => outcome,
                    OwnedTaskOutcome::TimedOut => OperationOutcome::failed(
                        operation,
                        &request_id,
                        ExecutorError::RequestTimedOut {
                            operation,
                            timeout_ms,
                        },
                    ),
                    OwnedTaskOutcome::CancelledBeforeStart
                    | OwnedTaskOutcome::CancelledAfterGrace => OperationOutcome::failed(
                        operation,
                        &request_id,
                        ExecutorError::RequestCancelled { operation },
                    ),
                };
                (outcome, event_error)
            };
            let state = request_state_for_outcome(&outcome);
            let terminal_event_error = emit_request_state(
                artifacts,
                clock,
                context.cell_id(),
                context.trial_id(),
                &request_id,
                state,
            )
            .await
            .err();
            (outcome, event_error.or(terminal_event_error))
        },
    )
    .await;
    let mut outcomes = Vec::with_capacity(timed.len());
    for timed in timed {
        let (outcome, error) = timed.outcome;
        if let Some(error) = error {
            retain_scheduler_error(deferred_error, Err(error));
        }
        outcomes.push(TimedOperationOutcome {
            request_id: timed.request_id,
            outcome,
            start_offset_ns: timed.start_offset_ns,
            latency_ns: timed.latency_ns,
        });
    }
    outcomes
}

fn timed_request_observations(
    timed_outcomes: &[TimedOperationOutcome],
    cell_id: &str,
    trial_id: &str,
) -> Vec<RequestObservation> {
    timed_outcomes
        .iter()
        .map(|timed| RequestObservation {
            operation_id: timed.outcome.operation_id(),
            cell_id: cell_id.to_owned(),
            trial_id: trial_id.to_owned(),
            request_id: timed.outcome.request_id().to_owned(),
            start_offset_ns: timed.start_offset_ns,
            latency_ns: timed.latency_ns,
            succeeded: timed
                .outcome
                .response_metadata()
                .is_some_and(|metadata| metadata.status == ProductOutputStatus::Succeeded),
            status: timed.outcome.response_metadata().map_or_else(
                || {
                    timed.outcome.error().map_or_else(
                        || "failed".to_owned(),
                        |error| bounded_event_text(&error.to_string()),
                    )
                },
                |metadata| metadata.status.as_str().to_owned(),
            ),
            response_bytes: timed
                .outcome
                .response_metadata()
                .map_or(0, |metadata| metadata.response_bytes),
            bounded_response_sha256: timed
                .outcome
                .response_metadata()
                .map(|metadata| metadata.bounded_response_sha256.clone()),
        })
        .collect()
}

fn request_state_for_outcome(outcome: &OperationOutcome) -> RequestState {
    if matches!(
        outcome.error(),
        Some(ExecutorError::RequestCancelled { .. })
    ) {
        RequestState::Cancelled
    } else if outcome
        .response_metadata()
        .is_some_and(|metadata| metadata.status == ProductOutputStatus::Succeeded)
    {
        RequestState::Succeeded
    } else {
        RequestState::Failed
    }
}

fn work_state_for_phase(status: PhaseStatus) -> WorkState {
    match status {
        PhaseStatus::Succeeded => WorkState::Completed,
        PhaseStatus::Failed | PhaseStatus::TimedOut => WorkState::Failed,
        PhaseStatus::Cancelled => WorkState::Cancelled,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn retain_scheduler_error(
    retained: &mut Option<SchedulerError>,
    result: Result<(), SchedulerError>,
) {
    if retained.is_none() {
        *retained = result.err();
    }
}

#[allow(clippy::too_many_arguments)]
async fn destroy_trial_sandbox(
    startup: &StartupConfig,
    artifacts: &RunArtifacts,
    cell: &ExpandedCell,
    trial_id: &str,
    product: &ProductGateway,
    sandbox_id: &OwnedSandboxId,
    workspace_root: &Path,
    workspace_identity: &OwnedIdentity,
    workspaces: &mut BlockWorkspaceLedger,
) -> Result<(), SchedulerError> {
    let correlation = Correlation::new(
        &artifacts.run_id,
        &cell.cell_id,
        trial_id,
        "destroy-sandbox",
    )?;
    match tokio::time::timeout(
        SANDBOX_DESTROY_TIMEOUT,
        product.destroy_sandbox(sandbox_id, &correlation),
    )
    .await
    {
        Ok(Ok(())) => {
            workspaces.remove_trial_root(startup, workspace_root, workspace_identity)?;
            Ok(())
        }
        Ok(Err(error)) => Err(SchedulerError::Gateway(error)),
        Err(_) => Err(SchedulerError::SandboxDestroyTimeout(trial_id.to_owned())),
    }
}

#[allow(clippy::too_many_arguments)]
async fn finish_cancelled_setup(
    startup: &StartupConfig,
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell: &ExpandedCell,
    trial_id: &str,
    kind: TrialKind,
    sequence: u32,
    setup_started: Instant,
    product: &ProductGateway,
    workspace_root: &Path,
    workspace_identity: &OwnedIdentity,
    workspaces: &mut BlockWorkspaceLedger,
    cleanup: SetupCancellationCleanup,
) -> Result<TrialExecutionResult, SchedulerError> {
    let warmup = kind == TrialKind::Warmup;
    let setup_ns = elapsed_ns(setup_started);
    let mut deferred_error = None;
    for (phase, state) in [
        (LifecyclePhase::Setup, WorkState::Cancelled),
        (LifecyclePhase::Operation, WorkState::Skipped),
        (LifecyclePhase::Verify, WorkState::Skipped),
        (LifecyclePhase::Teardown, WorkState::Running),
    ] {
        retain_scheduler_error(
            &mut deferred_error,
            emit_trial_phase(
                artifacts,
                clock,
                &cell.cell_id,
                trial_id,
                warmup,
                phase,
                state,
            )
            .await,
        );
    }

    let teardown_started = Instant::now();
    let cleanup_result = match cleanup {
        SetupCancellationCleanup::RootOnly => workspaces
            .remove_trial_root(startup, workspace_root, workspace_identity)
            .map(|()| true),
        SetupCancellationCleanup::OwnedSandbox(sandbox_id) => destroy_trial_sandbox(
            startup,
            artifacts,
            cell,
            trial_id,
            product,
            &sandbox_id,
            workspace_root,
            workspace_identity,
            workspaces,
        )
        .await
        .map(|()| true),
        SetupCancellationCleanup::DeferredToGatewayOrCellOwner => Ok(false),
    };
    let teardown_ns = elapsed_ns(teardown_started);
    let cleanup_baseline_restored = cleanup_result
        .as_ref()
        .is_ok_and(|baseline_restored| *baseline_restored);
    let cleanup_failed = cleanup_result.is_err();
    if let Err(error) = cleanup_result {
        retain_scheduler_error(&mut deferred_error, Err(error));
    }

    retain_scheduler_error(
        &mut deferred_error,
        record_cancelled_setup(
            startup,
            artifacts,
            clock,
            cell,
            trial_id,
            kind,
            sequence,
            setup_ns,
            teardown_ns,
            workspace_root,
            cleanup_baseline_restored,
            cleanup_failed,
        )
        .await,
    );

    match deferred_error {
        Some(error) => Err(error),
        None => Ok(TrialExecutionResult::cancelled()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_setup_failure(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell: &ExpandedCell,
    trial_id: &str,
    kind: TrialKind,
    sequence: u32,
    setup_started: Instant,
    cleanup_baseline_restored: bool,
    error: SchedulerError,
) -> Result<TrialExecutionResult, SchedulerError> {
    let warmup = kind == TrialKind::Warmup;
    emit_trial_phase(
        artifacts,
        clock,
        &cell.cell_id,
        trial_id,
        warmup,
        LifecyclePhase::Setup,
        WorkState::Failed,
    )
    .await?;
    artifacts
        .append_observation(ObservationRecord::Trial(TrialSample {
            operation_id: cell.operation_id,
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            kind,
            sequence_in_cell: sequence,
            lifecycle: LifecycleDurations {
                setup_ns: elapsed_ns(setup_started),
                operation_ns: 0,
                verify_ns: 0,
                teardown_ns: 0,
            },
            product_succeeded: false,
            infrastructure_failed: true,
            cleanup_baseline_restored,
            correctness: fold_correctness(cell.operation_id, false, cleanup_baseline_restored, &[]),
            primary_operation_latency_ns: None,
            artifacts: Vec::new(),
        }))
        .await?;
    emit_trial_state(
        artifacts,
        clock,
        &cell.cell_id,
        trial_id,
        warmup,
        WorkState::Failed,
    )
    .await?;
    Ok(TrialExecutionResult::abort(error))
}

#[allow(clippy::too_many_arguments)]
async fn record_cancelled_setup(
    startup: &StartupConfig,
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell: &ExpandedCell,
    trial_id: &str,
    kind: TrialKind,
    sequence: u32,
    setup_ns: u64,
    teardown_ns: u64,
    workspace_root: &Path,
    cleanup_baseline_restored: bool,
    cleanup_failed: bool,
) -> Result<(), SchedulerError> {
    let warmup = kind == TrialKind::Warmup;
    let mut deferred_error = None;
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_phase(
            artifacts,
            clock,
            &cell.cell_id,
            trial_id,
            warmup,
            LifecyclePhase::Teardown,
            if cleanup_baseline_restored {
                WorkState::Completed
            } else if cleanup_failed {
                WorkState::Failed
            } else {
                WorkState::Cancelled
            },
        )
        .await,
    );
    match collect_resource_snapshot(clock, workspace_root, &startup.paths.root).await {
        Ok(readings) => {
            for reading in readings {
                retain_scheduler_error(
                    &mut deferred_error,
                    persist_resource(artifacts, clock, cell, trial_id, reading).await,
                );
            }
        }
        Err(error) => retain_scheduler_error(&mut deferred_error, Err(error)),
    }
    retain_scheduler_error(
        &mut deferred_error,
        artifacts
            .append_observation(ObservationRecord::Trial(TrialSample {
                operation_id: cell.operation_id,
                cell_id: cell.cell_id.clone(),
                trial_id: trial_id.to_owned(),
                kind,
                sequence_in_cell: sequence,
                lifecycle: LifecycleDurations {
                    setup_ns,
                    operation_ns: 0,
                    verify_ns: 0,
                    teardown_ns,
                },
                product_succeeded: false,
                infrastructure_failed: cleanup_failed,
                cleanup_baseline_restored,
                correctness: fold_correctness(
                    cell.operation_id,
                    false,
                    cleanup_baseline_restored,
                    &[],
                ),
                primary_operation_latency_ns: None,
                artifacts: Vec::new(),
            }))
            .await,
    );
    retain_scheduler_error(
        &mut deferred_error,
        emit_trial_state(
            artifacts,
            clock,
            &cell.cell_id,
            trial_id,
            warmup,
            WorkState::Cancelled,
        )
        .await,
    );
    deferred_error.map_or(Ok(()), Err)
}

async fn collect_resource_snapshot(
    clock: &MonotonicClock,
    workspace_root: &Path,
    host_root: &Path,
) -> Result<Vec<ResourceReading>, SchedulerError> {
    let at = MonotonicInstant::from_offset_ns(clock.offset_ns());
    let workspace_root = workspace_root.to_path_buf();
    let host_root = host_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut readings = Vec::new();
        readings
            .extend(ProcessCollector::attach(ProcessScope::Runner, std::process::id()).read(at));
        readings.extend(VolumeCollector::new(VolumeScope::Workspace, workspace_root).read(at));
        readings.extend(HostVolumeCollector::new(host_root).read(at));
        readings
    })
    .await
    .map_err(|error| SchedulerError::ResourceTask(error.to_string()))
}

async fn persist_resource(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell: &ExpandedCell,
    trial_id: &str,
    reading: ResourceReading,
) -> Result<(), SchedulerError> {
    let (value, unavailable_reason) = match &reading.value {
        Availability::Available { value } => (Some(*value), None),
        Availability::Unavailable { source, reason } => (None, Some(format!("{source}:{reason}"))),
    };
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::ResourceWindow {
                cell_id: cell.cell_id.clone(),
                trial_id: trial_id.to_owned(),
                metric_id: reading.metric_id.clone(),
                value,
                unavailable_reason,
            },
        )
        .await?;
    artifacts
        .append_observation(ObservationRecord::Resource(ResourceObservation {
            cell_id: cell.cell_id.clone(),
            trial_id: trial_id.to_owned(),
            request_id: None,
            reading,
        }))
        .await
}

async fn persist_check(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    check: &CheckResult,
) -> Result<(), SchedulerError> {
    let check_id = serde_json::to_value(check.id)
        .map_err(|error| SchedulerError::ArtifactTask(error.to_string()))?
        .as_str()
        .ok_or_else(|| SchedulerError::ArtifactTask("check id is not a string".to_owned()))?
        .to_owned();
    if check.evidence.items.is_empty() {
        artifacts
            .events
            .emit(
                clock.offset_ns(),
                EventData::Correctness {
                    cell_id: check.cell_id.clone(),
                    trial_id: check.trial_id.clone(),
                    check_id,
                    passed: check.verdict == CheckVerdict::Pass,
                    expected: String::new(),
                    actual: String::new(),
                    artifact_id: None,
                },
            )
            .await?;
    } else {
        for evidence in &check.evidence.items {
            artifacts
                .events
                .emit(
                    clock.offset_ns(),
                    EventData::Correctness {
                        cell_id: check.cell_id.clone(),
                        trial_id: check.trial_id.clone(),
                        check_id: check_id.clone(),
                        passed: check.verdict == CheckVerdict::Pass,
                        expected: bounded_event_text(&evidence.expected),
                        actual: bounded_event_text(&evidence.actual),
                        artifact_id: evidence.artifact_id.clone(),
                    },
                )
                .await?;
        }
    }
    artifacts
        .append_observation(ObservationRecord::Check(check.clone()))
        .await
}

async fn emit_trial_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell_id: &str,
    trial_id: &str,
    warmup: bool,
    state: WorkState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::TrialState {
                cell_id: cell_id.to_owned(),
                trial_id: trial_id.to_owned(),
                warmup,
                state,
            },
        )
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn emit_trial_phase(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell_id: &str,
    trial_id: &str,
    warmup: bool,
    phase: LifecyclePhase,
    state: WorkState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::TrialPhase {
                cell_id: cell_id.to_owned(),
                trial_id: trial_id.to_owned(),
                warmup,
                phase,
                state,
            },
        )
        .await?;
    Ok(())
}

async fn emit_request_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell_id: &str,
    trial_id: &str,
    request_id: &str,
    state: RequestState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::RequestState {
                cell_id: cell_id.to_owned(),
                trial_id: trial_id.to_owned(),
                request_id: request_id.to_owned(),
                state,
            },
        )
        .await?;
    Ok(())
}

fn gateway_log_summary(logs: &crate::gateway::GatewayLogs) -> String {
    format!(
        "gateway logs retained as digests: stdout_bytes={},stdout_sha256=sha256:{:x},stdout_truncated={},stderr_bytes={},stderr_sha256=sha256:{:x},stderr_truncated={}",
        logs.stdout.len(),
        Sha256::digest(logs.stdout.as_bytes()),
        logs.stdout_truncated,
        logs.stderr.len(),
        Sha256::digest(logs.stderr.as_bytes()),
        logs.stderr_truncated
    )
}

fn gateway_log_chunk_messages(stream: &str, contents: &str, truncated: bool) -> Vec<String> {
    if contents.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < contents.len() {
        let mut end = (start + GATEWAY_LOG_EVENT_PAYLOAD_BYTES).min(contents.len());
        while !contents.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        chunks.push(&contents[start..end]);
        start = end;
    }

    let count = chunks.len();
    let digest = Sha256::digest(contents.as_bytes());
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            format!(
                "gateway_log/v1 stream={stream} chunk={}/{} truncated={truncated} sha256=sha256:{digest:x}\n{chunk}",
                index + 1,
                count,
            )
        })
        .collect()
}

async fn emit_gateway_logs(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    logs: &crate::gateway::GatewayLogs,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::Log {
                level: LogLevel::Info,
                message: bounded_event_text(&gateway_log_summary(logs)),
            },
        )
        .await?;
    for message in gateway_log_chunk_messages("stdout", &logs.stdout, logs.stdout_truncated)
        .into_iter()
        .chain(gateway_log_chunk_messages(
            "stderr",
            &logs.stderr,
            logs.stderr_truncated,
        ))
    {
        artifacts
            .events
            .emit(
                clock.offset_ns(),
                EventData::Log {
                    level: LogLevel::Info,
                    message,
                },
            )
            .await?;
    }
    Ok(())
}

async fn emit_family_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    family: FamilyId,
    state: WorkState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::FamilyState {
                family: family_name(family).to_owned(),
                state,
            },
        )
        .await?;
    Ok(())
}

async fn emit_cell_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    cell_id: &str,
    state: WorkState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::CellState {
                cell_id: cell_id.to_owned(),
                state,
            },
        )
        .await?;
    Ok(())
}

async fn ensure_failure_observation(
    expanded: &ExpandedPlan,
    artifacts: &RunArtifacts,
) -> Result<(), SchedulerError> {
    if *artifacts.observation_sequence.lock().await != 0 {
        return Ok(());
    }
    let Some(cell) = expanded.cells.first() else {
        return Ok(());
    };
    artifacts
        .append_observation(ObservationRecord::Trial(TrialSample {
            operation_id: cell.operation_id,
            cell_id: cell.cell_id.clone(),
            trial_id: "campaign-preparation".to_owned(),
            kind: TrialKind::Measured,
            sequence_in_cell: 0,
            lifecycle: LifecycleDurations {
                setup_ns: 0,
                operation_ns: 0,
                verify_ns: 0,
                teardown_ns: 0,
            },
            product_succeeded: false,
            infrastructure_failed: true,
            cleanup_baseline_restored: false,
            correctness: fold_correctness(cell.operation_id, false, false, &[]),
            primary_operation_latency_ns: None,
            artifacts: Vec::new(),
        }))
        .await
}

async fn emit_run_state(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    state: RunState,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(clock.offset_ns(), EventData::RunState { state })
        .await?;
    Ok(())
}

async fn transition_to_cancelling(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
) -> Result<(), SchedulerError> {
    let state = artifacts.manifest()?.state;
    if is_terminal(state) {
        return Ok(());
    }
    if state != RunState::Cancelling {
        artifacts.transition(RunState::Cancelling, None)?;
    }
    // The cancellation API may have won the durable manifest race. The
    // scheduler still owns ordered event projection, so emit the state before
    // the terminal cancellation event in either case.
    emit_run_state(artifacts, clock, RunState::Cancelling).await?;
    Ok(())
}

fn cancellation_preempted_transition(error: &SchedulerError, target: RunState) -> bool {
    matches!(
        error,
        SchedulerError::InvalidManifestTransition {
            from: RunState::Cancelling,
            to,
        } if *to == target
    )
}

fn scheduler_failure_code(error: &SchedulerError) -> &'static str {
    match error {
        SchedulerError::Fixture(_) => "fixture_failure",
        SchedulerError::Cleanup(_) | SchedulerError::CleanupBaseline { .. } => "cleanup_failure",
        SchedulerError::Gateway(_) => "gateway_failure",
        SchedulerError::Artifact(_) | SchedulerError::Event(_) | SchedulerError::Report(_) => {
            "artifact_failure"
        }
        SchedulerError::UnsupportedClientCohort(_) => "unsupported_client_cohort",
        SchedulerError::InvocationCountMismatch { .. } => "harness_integrity_failure",
        SchedulerError::InvalidPlan(_) | SchedulerError::InvalidExecutionBlock(_) => {
            "invalid_expanded_plan"
        }
        _ => "infrastructure_failure",
    }
}

async fn emit_warning(
    artifacts: &RunArtifacts,
    clock: &MonotonicClock,
    code: &str,
    message: &str,
) -> Result<(), SchedulerError> {
    artifacts
        .events
        .emit(
            clock.offset_ns(),
            EventData::Warning {
                code: code.to_owned(),
                message: bounded_event_text(message),
            },
        )
        .await?;
    Ok(())
}

fn bounded_event_text(value: &str) -> String {
    if value.len() <= MAX_SCHEDULER_EVENT_TEXT_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_SCHEDULER_EVENT_TEXT_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...", &value[..end])
}

fn bounded_failure_code(value: &str) -> String {
    let mut bounded = value
        .bytes()
        .take(MAX_FAILURE_CODE_BYTES)
        .map(|byte| match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => char::from(byte),
            b'A'..=b'Z' => char::from(byte.to_ascii_lowercase()),
            _ => '_',
        })
        .collect::<String>();
    if bounded.is_empty() {
        bounded.push_str("campaign_failed");
    }
    bounded
}

#[cfg(test)]
mod gateway_log_event_tests {
    use super::*;

    #[test]
    fn gateway_log_events_retain_every_utf8_byte_with_a_bounded_versioned_header() {
        let contents = format!(
            "gateway startup complete\\n[redacted sensitive gateway log line]\\n{}",
            "界".repeat(GATEWAY_LOG_EVENT_PAYLOAD_BYTES)
        );
        let messages = gateway_log_chunk_messages("stderr", &contents, true);
        assert!(messages.len() > 1);
        assert!(messages
            .iter()
            .all(|message| message.len() <= MAX_SCHEDULER_EVENT_TEXT_BYTES));
        assert!(messages
            .iter()
            .all(|message| message.starts_with("gateway_log/v1 stream=stderr chunk=")));
        assert!(messages
            .iter()
            .all(|message| message.contains("truncated=true")));

        let retained = messages
            .iter()
            .map(|message| message.split_once('\n').expect("chunk delimiter").1)
            .collect::<String>();
        assert_eq!(retained, contents);
    }

    #[test]
    fn empty_gateway_stream_does_not_create_a_spurious_log_event() {
        assert!(gateway_log_chunk_messages("stdout", "", false).is_empty());
    }
}

#[cfg(test)]
mod manifest_transition_tests {
    use super::*;

    #[test]
    fn manifest_lifecycle_allows_only_documented_edges() {
        let states = [
            RunState::Planned,
            RunState::Queued,
            RunState::Preparing,
            RunState::Running,
            RunState::Verifying,
            RunState::TearingDown,
            RunState::Cancelling,
            RunState::Completed,
            RunState::Failed,
            RunState::Cancelled,
        ];
        let allowed = [
            (RunState::Planned, RunState::Queued),
            (RunState::Queued, RunState::Preparing),
            (RunState::Preparing, RunState::Running),
            (RunState::Running, RunState::Verifying),
            (RunState::Verifying, RunState::TearingDown),
            (RunState::TearingDown, RunState::Completed),
            (RunState::Planned, RunState::Cancelling),
            (RunState::Queued, RunState::Cancelling),
            (RunState::Preparing, RunState::Cancelling),
            (RunState::Running, RunState::Cancelling),
            (RunState::Verifying, RunState::Cancelling),
            (RunState::TearingDown, RunState::Cancelling),
            (RunState::Cancelling, RunState::Cancelled),
            (RunState::Planned, RunState::Failed),
            (RunState::Queued, RunState::Failed),
            (RunState::Preparing, RunState::Failed),
            (RunState::Running, RunState::Failed),
            (RunState::Verifying, RunState::Failed),
            (RunState::TearingDown, RunState::Failed),
        ];

        for from in states {
            for to in states {
                assert_eq!(
                    valid_manifest_transition(from, to),
                    allowed.contains(&(from, to)),
                    "unexpected transition {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn cancellation_preemption_is_recognized_for_scheduler_progression() {
        let preparing = SchedulerError::InvalidManifestTransition {
            from: RunState::Cancelling,
            to: RunState::Preparing,
        };
        assert!(cancellation_preempted_transition(
            &preparing,
            RunState::Preparing
        ));
        assert!(!cancellation_preempted_transition(
            &preparing,
            RunState::Running
        ));
        assert!(cancellation_preempted_transition(
            &SchedulerError::InvalidManifestTransition {
                from: RunState::Cancelling,
                to: RunState::Verifying,
            },
            RunState::Verifying
        ));
        assert!(cancellation_preempted_transition(
            &SchedulerError::InvalidManifestTransition {
                from: RunState::Cancelling,
                to: RunState::TearingDown,
            },
            RunState::TearingDown
        ));

        let regression = SchedulerError::InvalidManifestTransition {
            from: RunState::Running,
            to: RunState::Preparing,
        };
        assert!(!cancellation_preempted_transition(
            &regression,
            RunState::Preparing
        ));
    }

    #[test]
    fn campaign_reservation_is_planned_until_durable_queue_admission() {
        let gate = CampaignGate::default();
        let (run_id, _, replayed) = gate.reserve("planned-boundary").expect("reserve run");
        assert!(!replayed);
        assert_eq!(
            gate.active().expect("read active campaign"),
            Some((run_id.clone(), RunState::Planned))
        );

        gate.rollback_reservation("planned-boundary", &run_id)
            .expect("roll back pre-admission reservation");
        assert_eq!(gate.active().expect("read empty gate"), None);

        let (run_id, _, replayed) = gate.reserve("queued-boundary").expect("reserve run");
        assert!(!replayed);
        gate.update_state(&run_id, RunState::Queued)
            .expect("mark durable queue admission");
        assert!(matches!(
            gate.rollback_reservation("queued-boundary", &run_id),
            Err(CampaignGateError::ReservationMismatch { .. })
        ));
    }
}

#[cfg(test)]
mod layerstack_observation_tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::executors::layerstack::{
        SessionDispositionCounts, SourceLayerAllocation, SquashLayerstackPartialEvidence,
    };

    #[allow(clippy::too_many_arguments)]
    fn span_node(
        id: &str,
        parent: Option<&str>,
        name: &str,
        offset_ms: f64,
        duration_ms: f64,
        status: &str,
        attrs: Value,
        children: Vec<Value>,
    ) -> Value {
        json!({
            "span": {
                "ts": 1,
                "trace": "trace-1",
                "span": id,
                "parent": parent,
                "name": name,
                "dur_ms": duration_ms,
                "status": status,
                "attrs": attrs,
            },
            "offset_ms": offset_ms,
            "children": children,
            "events": [],
        })
    }

    fn exact_layerstack_trace() -> ProductTrace {
        let remount_one = span_node(
            "remount-1",
            Some("sweep"),
            "workspace_session.remount",
            7.0,
            1.0,
            "completed",
            json!({"session_id": "session-1", "disposition": "migrated"}),
            Vec::new(),
        );
        let remount_two = span_node(
            "remount-2",
            Some("sweep"),
            "workspace_session.remount",
            8.0,
            1.5,
            "completed",
            json!({"session_id": "session-2", "disposition": "identity"}),
            Vec::new(),
        );
        let squash = span_node(
            "squash",
            Some("dispatch"),
            "layerstack.squash",
            1.0,
            10.0,
            "completed",
            json!({
                "manifest_version": 9,
                "s2_root_hash": "sha256:s2",
                "s2_layer_count": 1,
                "s2_active_logical_bytes": 10,
                "s2_active_allocated_bytes": 12,
                "s2_storage_logical_bytes": 20,
                "s2_storage_allocated_bytes": 24,
                "s2_staging_entry_count": 0,
            }),
            vec![
                span_node(
                    "plan",
                    Some("squash"),
                    "layerstack.squash.plan",
                    1.1,
                    0.5,
                    "completed",
                    json!({}),
                    Vec::new(),
                ),
                span_node(
                    "flatten",
                    Some("squash"),
                    "layerstack.squash.flatten",
                    2.0,
                    2.0,
                    "completed",
                    json!({}),
                    Vec::new(),
                ),
                span_node(
                    "commit",
                    Some("squash"),
                    "layerstack.squash.commit",
                    4.0,
                    2.0,
                    "completed",
                    json!({}),
                    Vec::new(),
                ),
                span_node(
                    "sweep",
                    Some("squash"),
                    "layerstack.squash.remount_sweep",
                    6.5,
                    3.0,
                    "completed",
                    json!({"sessions": 2, "width": 4}),
                    vec![remount_one, remount_two],
                ),
            ],
        );
        let root = span_node(
            "dispatch",
            None,
            "daemon.dispatch",
            0.0,
            12.0,
            "completed",
            json!({}),
            vec![squash],
        );
        ProductTrace {
            trace_id: "trace-1".to_owned(),
            spans: vec![serde_json::from_value(root).expect("strict trace node")],
        }
    }

    #[test]
    fn exact_trace_maps_registered_phases_and_commit_boundary() {
        let trace = exact_layerstack_trace();
        let collected = collect_layerstack_trace(Ok(&trace), "cell-1", "trial-1", Some(100), 2);

        assert!(collected.warnings.is_empty(), "{:?}", collected.warnings);
        assert_eq!(collected.phases.len(), 7);
        assert_eq!(
            collected
                .phases
                .iter()
                .filter(|phase| phase.id == PhaseId::WorkspaceSessionRemount)
                .count(),
            2
        );
        for phase in &collected.phases {
            assert_eq!(phase.unit, PhaseUnit::Nanoseconds);
            assert_eq!(
                phase.correlation,
                PhaseCorrelationRule::ExactRequestTraceSpan
            );
            assert_eq!(phase.request_id.as_deref(), Some(LAYERSTACK_REQUEST_ID));
            assert_eq!(phase.status, PhaseStatus::Succeeded);
        }
        let commit = collected
            .phases
            .iter()
            .find(|phase| phase.id == PhaseId::LayerstackCommit)
            .expect("commit phase");
        assert_eq!(commit.start_offset_ns, 4_000_100);
        assert_eq!(commit.duration_ns, 2_000_000);
        assert_eq!(
            collected.s2_post_commit.monotonic_offset_ns,
            Availability::Available { value: 6_000_100 }
        );
        assert_eq!(
            collected.s2_post_commit.root_hash,
            Availability::Available {
                value: "sha256:s2".to_owned()
            }
        );
        assert_eq!(
            collected.s2_post_commit.storage_allocated_bytes,
            Availability::Available { value: 24 }
        );
    }

    #[test]
    fn ambiguous_registered_span_is_warned_and_never_synthesized() {
        let mut trace = exact_layerstack_trace();
        let duplicate: ProductTraceSpanNode = serde_json::from_value(span_node(
            "plan-duplicate",
            Some("squash"),
            "layerstack.squash.plan",
            1.2,
            0.4,
            "completed",
            json!({}),
            Vec::new(),
        ))
        .expect("strict trace node");
        trace.spans[0].children[0].children.push(duplicate);

        let collected = collect_layerstack_trace(Ok(&trace), "cell-1", "trial-1", Some(0), 2);
        assert!(collected
            .warnings
            .iter()
            .any(|warning| warning.contains("matched 2 spans; expected 1")));
        assert!(!collected
            .phases
            .iter()
            .any(|phase| phase.id == PhaseId::LayerstackStoragePlan));
        assert!(collected
            .phases
            .iter()
            .any(|phase| phase.id == PhaseId::LayerstackFlatten));
    }

    #[test]
    fn wrong_trace_root_yields_only_explicit_unavailability() {
        let mut trace = exact_layerstack_trace();
        trace.spans[0].span.name = "unregistered.root".to_owned();

        let collected = collect_layerstack_trace(Ok(&trace), "cell-1", "trial-1", Some(0), 2);
        assert!(collected.phases.is_empty());
        assert!(matches!(
            collected.s2_post_commit.storage_allocated_bytes,
            Availability::Unavailable { .. }
        ));
        assert_eq!(collected.warnings.len(), 1);
    }

    #[test]
    fn millisecond_conversion_is_checked_and_rounded_once() {
        assert_eq!(milliseconds_to_nanoseconds(0.0), Ok(0));
        assert_eq!(milliseconds_to_nanoseconds(1.25), Ok(1_250_000));
        assert_eq!(milliseconds_to_nanoseconds(0.000_000_6), Ok(1));
        assert!(milliseconds_to_nanoseconds(-1.0).is_err());
        assert!(milliseconds_to_nanoseconds(f64::NAN).is_err());
        assert!(milliseconds_to_nanoseconds(f64::INFINITY).is_err());
        let overflow_milliseconds = (u64::MAX as f64) / 1_000_000.0;
        assert!(milliseconds_to_nanoseconds(overflow_milliseconds).is_err());
    }

    #[test]
    fn reclaimed_allocation_subtracts_only_settled_retained_sources() {
        let partial = SquashLayerstackPartialEvidence {
            requested_live_sessions: 1,
            observed_migrated_sessions: 0,
            observed_non_migrated_sessions: 1,
            dispositions: SessionDispositionCounts {
                migrated: 0,
                identity: 0,
                leased: 1,
                faulty: 0,
                session_gone: 0,
            },
            manifest_version: 2,
            squashed_block_count: 1,
            replaced_layer_count: 2,
            source_layer_ids: vec!["source-a".to_owned(), "source-b".to_owned()],
            retained_source_layer_ids: vec!["source-b".to_owned()],
            manifest_reduced: true,
            content_equivalent: true,
            usable_session_count: 1,
        };
        let allocations = vec![
            SourceLayerAllocation {
                layer_id: "source-a".to_owned(),
                logical_bytes: Availability::Available { value: 8 },
                allocated_bytes: Availability::Available { value: 12 },
            },
            SourceLayerAllocation {
                layer_id: "source-b".to_owned(),
                logical_bytes: Availability::Available { value: 8 },
                allocated_bytes: Availability::Available { value: 20 },
            },
        ];
        assert_eq!(
            reclaimed_bytes(&partial, &allocations),
            Availability::Available { value: 12 }
        );

        let unavailable_allocations = vec![SourceLayerAllocation {
            layer_id: "source-a".to_owned(),
            logical_bytes: Availability::Available { value: 8 },
            allocated_bytes: unavailable("test", "counter unavailable"),
        }];
        assert!(matches!(
            reclaimed_bytes(&partial, &unavailable_allocations),
            Availability::Unavailable { .. }
        ));
    }
}

#[cfg(test)]
mod product_resource_observation_tests {
    use super::*;

    fn available_sample(sampled: bool) -> ProductResourceQuerySample {
        ProductResourceQuerySample {
            monotonic_offset_ns: 42,
            sampled,
            resources: Ok(ProductSandboxResources {
                observed_unix_ms: 1_000,
                cpu_usage_usec: Some(7),
                memory_current_bytes: Some(11),
                memory_limit_bytes: Some(100),
                io_read_bytes: Some(13),
                io_write_bytes: Some(17),
            }),
            storage: Ok(ProductStorageResources {
                daemon_container_pid: 42,
                layerstack_storage_allocated_bytes: Some(19),
                upperdir: ProductUpperdirAllocation::Available {
                    allocated_bytes: 23,
                    workspace_count: 2,
                },
            }),
        }
    }

    fn reading<'a>(readings: &'a [ResourceReading], id: &str) -> &'a ResourceReading {
        readings
            .iter()
            .find(|reading| reading.metric_id == id)
            .expect("registered reading")
    }

    #[test]
    fn product_counters_map_to_registered_units_and_sampled_peak() {
        let readings = product_resource_readings(available_sample(false));
        assert_eq!(readings.len(), 9);
        assert_eq!(
            reading(&readings, SANDBOX_MEMORY_CURRENT.id).value,
            Availability::Available { value: 11.0 }
        );
        let peak = reading(&readings, SANDBOX_MEMORY_PEAK.id);
        assert_eq!(peak.value, Availability::Available { value: 11.0 });
        assert!(peak.sampled, "memory peak is a sampled-current fallback");
        assert_eq!(
            reading(&readings, SANDBOX_CPU_TIME.id).value,
            Availability::Available { value: 7_000.0 }
        );
        assert_eq!(
            reading(&readings, SANDBOX_BLOCK_READ.id).value,
            Availability::Available { value: 13.0 }
        );
        assert_eq!(
            reading(&readings, SANDBOX_BLOCK_WRITE.id).value,
            Availability::Available { value: 17.0 }
        );
        assert_eq!(
            reading(&readings, LAYERSTACK_BYTES.id).value,
            Availability::Available { value: 19.0 }
        );
        assert_eq!(
            reading(&readings, UPPERDIR_BYTES.id).value,
            Availability::Available { value: 23.0 }
        );
        assert!(matches!(
            &reading(&readings, DAEMON_RSS.id).value,
            Availability::Unavailable { reason, .. }
                if reason.contains("container PID 42") && reason.contains("start identity")
        ));
        assert!(readings
            .iter()
            .all(|reading| reading.monotonic_offset_ns == 42));
    }

    #[test]
    fn absent_or_failed_product_counters_are_never_zero() {
        let mut sample = available_sample(true);
        sample.resources = Ok(ProductSandboxResources {
            observed_unix_ms: 1_000,
            cpu_usage_usec: None,
            memory_current_bytes: None,
            memory_limit_bytes: None,
            io_read_bytes: None,
            io_write_bytes: None,
        });
        let readings = product_resource_readings(sample);
        for definition in [
            SANDBOX_MEMORY_CURRENT,
            SANDBOX_MEMORY_PEAK,
            SANDBOX_CPU_TIME,
            SANDBOX_BLOCK_READ,
            SANDBOX_BLOCK_WRITE,
        ] {
            assert!(matches!(
                &reading(&readings, definition.id).value,
                Availability::Unavailable { .. }
            ));
        }

        let readings = product_resource_readings(ProductResourceQuerySample {
            monotonic_offset_ns: 7,
            sampled: false,
            resources: Err("query failed".to_owned()),
            storage: Err("query failed".to_owned()),
        });
        assert_eq!(readings.len(), 9);
        assert!(readings.iter().all(|reading| matches!(
            &reading.value,
            Availability::Unavailable { reason, .. } if reason == "query failed"
        )));
    }

    #[test]
    fn cpu_unit_conversion_overflow_is_explicitly_unavailable() {
        let mut sample = available_sample(false);
        sample
            .resources
            .as_mut()
            .expect("available resources")
            .cpu_usage_usec = Some(u64::MAX);
        let readings = product_resource_readings(sample);
        assert!(matches!(
            &reading(&readings, SANDBOX_CPU_TIME.id).value,
            Availability::Unavailable { reason, .. } if reason.contains("overflow")
        ));
    }

    #[test]
    fn incomplete_upperdir_allocation_is_unavailable_not_zero() {
        let mut sample = available_sample(true);
        sample.storage = Ok(ProductStorageResources {
            daemon_container_pid: 7,
            layerstack_storage_allocated_bytes: None,
            upperdir: ProductUpperdirAllocation::Unavailable {
                reason: "workspace ws-1 allocation walk was truncated".to_owned(),
            },
        });

        let readings = product_resource_readings(sample);
        assert!(matches!(
            &reading(&readings, UPPERDIR_BYTES.id).value,
            Availability::Unavailable { reason, .. } if reason.contains("truncated")
        ));
        assert!(matches!(
            &reading(&readings, LAYERSTACK_BYTES.id).value,
            Availability::Unavailable { reason, .. } if reason.contains("not reported")
        ));
    }
}

#[cfg(test)]
mod cancellation_boundary_tests {
    use super::*;
    use tokio::sync::oneshot;

    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    async fn pending_owned_task(started: oneshot::Sender<()>, dropped: oneshot::Sender<()>) {
        let _notify = NotifyOnDrop(Some(dropped));
        let _ = started.send(());
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn cancellation_before_first_poll_never_starts_owned_work() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let polled = Arc::new(AtomicBool::new(false));
        let task_polled = Arc::clone(&polled);

        let outcome =
            await_owned_task_with_grace(&cancellation, None, Duration::from_secs(1), async move {
                task_polled.store(true, Ordering::Relaxed);
                3_u8
            })
            .await;

        assert_eq!(outcome, OwnedTaskOutcome::CancelledBeforeStart);
        assert!(!polled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn cancel_during_sandbox_create_waits_grace_then_drops_owned_task() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            await_owned_task_with_grace(
                &task_cancellation,
                Some(SANDBOX_CREATE_TIMEOUT),
                Duration::from_millis(25),
                pending_owned_task(started_tx, dropped_tx),
            )
            .await
        });

        started_rx.await.expect("create task starts");
        cancellation.cancel();

        assert_eq!(
            task.await.expect("create boundary task joins"),
            OwnedTaskOutcome::CancelledAfterGrace
        );
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("create future is dropped promptly")
            .expect("create future is terminated");
    }

    #[tokio::test]
    async fn cancel_during_prepare_retains_completion_for_reverse_teardown() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            await_owned_task_with_grace(
                &task_cancellation,
                None,
                Duration::from_secs(1),
                async move {
                    let _ = started_tx.send(());
                    release_rx.await.expect("release prepared value");
                    7_u8
                },
            )
            .await
        });

        started_rx.await.expect("prepare task starts");
        cancellation.cancel();
        release_tx.send(()).expect("complete prepare in grace");

        assert_eq!(
            task.await.expect("prepare boundary task joins"),
            OwnedTaskOutcome::CancelledCompleted(7)
        );
    }

    #[tokio::test]
    async fn cancel_during_in_flight_request_waits_grace_then_drops_owned_task() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            await_owned_task_with_grace(
                &task_cancellation,
                Some(Duration::from_secs(60)),
                Duration::from_millis(25),
                pending_owned_task(started_tx, dropped_tx),
            )
            .await
        });

        started_rx.await.expect("request task starts");
        cancellation.cancel();

        assert_eq!(
            task.await.expect("request boundary task joins"),
            OwnedTaskOutcome::CancelledAfterGrace
        );
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("request future is dropped promptly")
            .expect("request future is terminated");
    }

    #[tokio::test]
    async fn cancel_during_verify_retains_completion_before_teardown() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            await_owned_task_with_grace(
                &task_cancellation,
                None,
                Duration::from_secs(1),
                async move {
                    let _ = started_tx.send(());
                    release_rx.await.expect("release verification");
                    11_u8
                },
            )
            .await
        });

        started_rx.await.expect("verification starts");
        cancellation.cancel();
        release_tx.send(()).expect("complete verify in grace");

        assert_eq!(
            task.await.expect("verify boundary task joins"),
            OwnedTaskOutcome::CancelledCompleted(11)
        );
    }

    #[test]
    fn operation_grace_policy_is_exhaustive_and_layerstack_specific() {
        assert_eq!(
            CancellationBoundary::SandboxCreate.grace(),
            DEFAULT_CANCELLATION_GRACE
        );
        for operation in OperationId::ALL {
            let expected = if operation == OperationId::SquashLayerstack {
                LAYERSTACK_CANCELLATION_GRACE
            } else {
                DEFAULT_CANCELLATION_GRACE
            };
            for boundary in [
                CancellationBoundary::OperationPrepare(operation),
                CancellationBoundary::OperationRequest(operation),
                CancellationBoundary::OperationVerify(operation),
            ] {
                assert_eq!(boundary.grace(), expected, "boundary={boundary:?}");
            }
        }
    }
}

#[cfg(test)]
mod cell_sandbox_lifecycle_tests {
    use super::*;

    #[test]
    fn cell_scoped_policies_share_a_sandbox_and_destructive_policies_do_not() {
        for policy in [
            ResolvedIsolationPolicy::ReusableVerifiedFixture,
            ResolvedIsolationPolicy::FreshSessionsPerTrial,
            ResolvedIsolationPolicy::PreparedSandboxPerCell,
        ] {
            assert!(requires_prepared_cell_sandbox(policy));
        }
        for policy in [
            ResolvedIsolationPolicy::FreshSandboxPerTrial,
            ResolvedIsolationPolicy::FreshTopologyPerTrial,
        ] {
            assert!(!requires_prepared_cell_sandbox(policy));
        }
    }
}
