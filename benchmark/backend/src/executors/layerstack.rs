use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::daemon_session::{CreatedSession, WorkspaceSessionLifecycle};
use crate::definitions::{
    CheckReference, ComparisonParticipation, ComparisonProjectionDefinition, FactorConstraint,
    FactorDefinition, FactorUnit, FactorValueKind, OperationDefinition, PhaseReference,
    FACTOR_SCHEMA_REVISION, OPERATION_SEMANTIC_REVISION, SUPPORTED_COHORTS,
};
use crate::gateway::ProductPath;
use crate::model::{
    validate_factor, validate_nonzero_u32, validate_nonzero_u64, validate_unit_ratio,
    AllowedNetworkProfile, CheckId, CleanupPolicy, CountSemantics, ExecutionShape, Factor,
    FactorId, FactorViolation, FamilyId, IsolationPolicy, OperationId, OperationValidationError,
    PhaseCorrelationRule, PhaseId, PhaseSource, PhaseUnit, ProductAccess, ProductOperation,
    ResolvedIsolationPolicy, SecurityClass, SessionActivity, UnitRatio,
};
use crate::resources::Availability;

use super::command::probe_session;
use super::{
    check_result, register_session, registered_sessions, response_metadata, session_registry,
    teardown_registered_sessions, ExecutorError, InvocationOutcome, OperationLifecycle,
    ProductOutputStatus, ResponseMetadata, RuntimeContext, RuntimeInvocation, RuntimeOutput,
    SessionRegistry, TeardownResult, Verification,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackFactors {
    pub live_sessions: Factor<u32>,
    pub requested_migration_ratio: Factor<UnitRatio>,
    pub remount_parallelism: Factor<u32>,
    pub squashable_blocks: Factor<u32>,
    pub layers_per_block: Factor<u32>,
    pub payload_bytes: Factor<u64>,
    pub session_activity: Factor<SessionActivity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackPlan {
    pub enabled: bool,
    pub factors: SquashLayerstackFactors,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackCell {
    pub live_sessions: u32,
    pub requested_migration_ratio: UnitRatio,
    pub remount_parallelism: u32,
    pub squashable_blocks: u32,
    pub layers_per_block: u32,
    pub payload_bytes: u64,
    pub session_activity: SessionActivity,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDisposition {
    Migrated,
    Identity,
    Leased,
    Faulty,
    SessionGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDispositionCounts {
    pub migrated: u32,
    pub identity: u32,
    pub leased: u32,
    pub faulty: u32,
    pub session_gone: u32,
}

impl SessionDispositionCounts {
    #[must_use]
    pub const fn total(self) -> u32 {
        self.migrated + self.identity + self.leased + self.faulty + self.session_gone
    }

    #[must_use]
    pub const fn non_migrated(self) -> u32 {
        self.identity + self.leased + self.faulty + self.session_gone
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSnapshot {
    pub monotonic_offset_ns: Availability<u64>,
    pub sampled: bool,
    pub manifest_version: Availability<u64>,
    pub root_hash: Availability<String>,
    pub active_layer_count: Availability<u64>,
    pub active_lease_count: Availability<u64>,
    pub active_logical_bytes: Availability<u64>,
    pub active_allocated_bytes: Availability<u64>,
    pub storage_logical_bytes: Availability<u64>,
    pub storage_allocated_bytes: Availability<u64>,
    pub staging_entry_count: Availability<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLayerAllocation {
    pub layer_id: String,
    pub logical_bytes: Availability<u64>,
    pub allocated_bytes: Availability<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackEvidence {
    pub requested_live_sessions: u32,
    pub observed_migrated_sessions: u32,
    pub observed_non_migrated_sessions: u32,
    pub dispositions: SessionDispositionCounts,
    pub effective_remount_parallelism: u32,
    pub observed_squashed_block_count: u32,
    pub observed_replaced_layer_count: u32,
    pub source_layer_ids: Vec<String>,
    pub retained_source_layer_ids: Vec<String>,
    pub source_layer_allocations: Vec<SourceLayerAllocation>,
    pub reclaimed_bytes: Availability<u64>,
    pub s0_baseline: StorageSnapshot,
    pub s1_sampled_peak: StorageSnapshot,
    pub s2_post_commit: StorageSnapshot,
    pub s3_settled: StorageSnapshot,
    pub manifest_reduced: bool,
    pub content_equivalent: bool,
    pub usable_session_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackComparisonIdentity {
    pub live_sessions: u32,
    pub requested_migration_ratio: UnitRatio,
    pub remount_parallelism: u32,
    pub squashable_blocks: u32,
    pub layers_per_block: u32,
    pub payload_bytes: u64,
    pub session_activity: SessionActivity,
}

const FACTORS: &[FactorDefinition] = &[
    FactorDefinition {
        id: FactorId::LiveSessions,
        label: "Live sessions N",
        help: "Prepared sessions exposed to the post-commit remount sweep; N is load, never request concurrency.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::NonNegative,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::RequestedMigrationRatio,
        label: "Requested migration ratio",
        help: "Fixture request for the share of live sessions eligible to migrate, rounded to the nearest whole session with halves upward; observed M remains evidence.",
        value_kind: FactorValueKind::UnitRatio,
        unit: Some(FactorUnit::Ratio),
        constraint: FactorConstraint::UnitInterval,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::RemountParallelism,
        label: "Remount parallelism W",
        help: "Fixed bounded width of the live-session remount sweep inside the single squash request.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::SquashableBlocks,
        label: "Squashable blocks B",
        help: "Number of contiguous deterministic layer blocks prepared for the squash request.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::LayersPerBlock,
        label: "Layers per block",
        help: "Published layers in each squashable block of the prepared topology.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::PayloadBytes,
        label: "Payload bytes",
        help: "Deterministically materialized payload bytes contributed by each published layer.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Bytes),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::SessionActivity,
        label: "Session activity",
        help: "Whether prepared live sessions remain idle or perform bounded deterministic activity before squash.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["idle", "active"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
];

const CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::LayerstackContentEquivalence,
        label: "Content equivalence",
        help: "Published content is unchanged across the atomic squash commit.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
    CheckReference {
        id: CheckId::LayerstackManifestReduction,
        label: "Manifest reduction",
        help: "The committed manifest has the expected reduced layer topology.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
    CheckReference {
        id: CheckId::LayerstackDispositionAccounting,
        label: "Session disposition accounting",
        help: "Migrated and non-migrated dispositions account for every requested live session.",
        semantic_revision: 1,
        evidence_limit: 64,
    },
    CheckReference {
        id: CheckId::LayerstackSessionUsability,
        label: "Session usability",
        help: "Sessions expected to survive the sweep remain usable after remount.",
        semantic_revision: 1,
        evidence_limit: 64,
    },
    CheckReference {
        id: CheckId::LayerstackResidue,
        label: "LayerStack residue",
        help: "Cleanup leaves no owned topology residue beyond the pre-trial baseline.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
];

const PHASES: &[PhaseReference] = &[
    PhaseReference {
        id: PhaseId::LayerstackSquash,
        label: "Total squash",
        help: "Server-observed total duration of the one squash request.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "layerstack.squash",
    },
    PhaseReference {
        id: PhaseId::LayerstackStoragePlan,
        label: "Storage plan",
        help: "Server phase that plans the layer-storage rewrite.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "layerstack.squash.plan",
    },
    PhaseReference {
        id: PhaseId::LayerstackFlatten,
        label: "Flatten",
        help: "Server phase that materializes flattened layer content.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "layerstack.squash.flatten",
    },
    PhaseReference {
        id: PhaseId::LayerstackCommit,
        label: "Commit",
        help: "Server phase that atomically commits the new manifest.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "layerstack.squash.commit",
    },
    PhaseReference {
        id: PhaseId::LayerstackRemountSweep,
        label: "Remount sweep",
        help: "Wall time of the bounded post-commit session sweep.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "layerstack.squash.remount_sweep",
    },
    PhaseReference {
        id: PhaseId::WorkspaceSessionRemount,
        label: "Session remount",
        help: "Per-session remount span observed within the bounded sweep.",
        semantic_revision: 1,
        unit: PhaseUnit::Nanoseconds,
        source: PhaseSource::ProductTrace,
        correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
        trace_span_name: "workspace_session.remount",
    },
];

const COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::LiveSessions,
    FactorId::RequestedMigrationRatio,
    FactorId::RemountParallelism,
    FactorId::SquashableBlocks,
    FactorId::LayersPerBlock,
    FactorId::PayloadBytes,
    FactorId::SessionActivity,
];

pub const DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::SquashLayerstack,
    family: FamilyId::LayerStack,
    label: "Squash LayerStack",
    help: "Measures the public manager squash_layerstacks operation, which forwards one internal squash and bounded remount sweep.",
    measured_boundary: "After a fresh deterministic topology and N live sessions are prepared, one squash request measures storage planning, flatten, commit, and the bounded remount sweep.",
    count_semantics_help: "Every measured trial issues exactly one product request; N counts prepared live-session load and never request concurrency.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::SingleRequestWithPreparedLoad {
        load_factor: FactorId::LiveSessions,
    },
    execution_shape: ExecutionShape::SingleRequestAfterPreparedLoad,
    isolation: IsolationPolicy::FreshTopologyPerTrial,
    cleanup: CleanupPolicy::DestroyTopologyAndVerifyBaseline,
    product_access: ProductAccess::PublicGateway(ProductOperation::SquashLayerstacks),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::DestructiveManagerMutation,
    factors: FACTORS,
    checks: CHECKS,
    phases: PHASES,
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: COMPARISON_FACTORS,
    },
};

#[must_use]
pub fn validate(plan: &SquashLayerstackPlan) -> Vec<OperationValidationError> {
    let operation = OperationId::SquashLayerstack;
    let factors = &plan.factors;
    let mut errors = validate_factor(operation, FactorId::LiveSessions, &factors.live_sessions);
    errors.extend(validate_factor(
        operation,
        FactorId::RequestedMigrationRatio,
        &factors.requested_migration_ratio,
    ));
    errors.extend(validate_unit_ratio(
        operation,
        FactorId::RequestedMigrationRatio,
        &factors.requested_migration_ratio,
    ));
    for (id, factor) in [
        (FactorId::RemountParallelism, &factors.remount_parallelism),
        (FactorId::SquashableBlocks, &factors.squashable_blocks),
        (FactorId::LayersPerBlock, &factors.layers_per_block),
    ] {
        errors.extend(validate_factor(operation, id, factor));
        errors.extend(validate_nonzero_u32(operation, id, factor));
    }
    errors.extend(validate_factor(
        operation,
        FactorId::PayloadBytes,
        &factors.payload_bytes,
    ));
    errors.extend(validate_nonzero_u64(
        operation,
        FactorId::PayloadBytes,
        &factors.payload_bytes,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::SessionActivity,
        &factors.session_activity,
    ));

    if factors
        .layers_per_block
        .values
        .iter()
        .any(|value| *value < 2)
    {
        push_validation_once(
            &mut errors,
            FactorId::LayersPerBlock,
            FactorViolation::IncompatibleCombination,
        );
    }
    for &blocks in &factors.squashable_blocks.values {
        for &layers_per_block in &factors.layers_per_block.values {
            if blocks
                .checked_mul(layers_per_block)
                .is_none_or(|layers| layers > MAX_PREPARED_LAYERS)
            {
                push_validation_once(
                    &mut errors,
                    FactorId::LayersPerBlock,
                    FactorViolation::SafetyBoundExceeded,
                );
            }
        }
        if blocks == 0 {
            continue;
        }
        for &live_sessions in &factors.live_sessions.values {
            for &ratio in &factors.requested_migration_ratio.values {
                if ratio.is_valid()
                    && requested_eligible_count(live_sessions, ratio) < blocks.saturating_sub(1)
                {
                    push_validation_once(
                        &mut errors,
                        FactorId::SquashableBlocks,
                        FactorViolation::IncompatibleCombination,
                    );
                }
            }
        }
    }
    errors
}

fn push_validation_once(
    errors: &mut Vec<OperationValidationError>,
    factor: FactorId,
    violation: FactorViolation,
) {
    let error = OperationValidationError {
        operation: OperationId::SquashLayerstack,
        factor,
        violation,
    };
    if !errors.contains(&error) {
        errors.push(error);
    }
}

#[must_use]
fn requested_eligible_count(live_sessions: u32, ratio: UnitRatio) -> u32 {
    debug_assert!(ratio.is_valid());
    let rounded = (f64::from(live_sessions) * ratio.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let eligible = rounded as u32;
    eligible.min(live_sessions)
}

pub fn expand(
    plan: &SquashLayerstackPlan,
) -> Result<Vec<SquashLayerstackCell>, Vec<OperationValidationError>> {
    let errors = validate(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }

    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &live_sessions in &factors.live_sessions.values {
        for &requested_migration_ratio in &factors.requested_migration_ratio.values {
            for &remount_parallelism in &factors.remount_parallelism.values {
                for &squashable_blocks in &factors.squashable_blocks.values {
                    for &layers_per_block in &factors.layers_per_block.values {
                        for &payload_bytes in &factors.payload_bytes.values {
                            for &session_activity in &factors.session_activity.values {
                                cells.push(SquashLayerstackCell {
                                    live_sessions,
                                    requested_migration_ratio,
                                    remount_parallelism,
                                    squashable_blocks,
                                    layers_per_block,
                                    payload_bytes,
                                    session_activity,
                                    resolved_isolation:
                                        ResolvedIsolationPolicy::FreshTopologyPerTrial,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(cells)
}

#[must_use]
pub const fn comparison_identity(
    cell: &SquashLayerstackCell,
) -> SquashLayerstackComparisonIdentity {
    SquashLayerstackComparisonIdentity {
        live_sessions: cell.live_sessions,
        requested_migration_ratio: cell.requested_migration_ratio,
        remount_parallelism: cell.remount_parallelism,
        squashable_blocks: cell.squashable_blocks,
        layers_per_block: cell.layers_per_block,
        payload_bytes: cell.payload_bytes,
        session_activity: cell.session_activity,
    }
}

pub(crate) const MAX_LAYER_PAYLOAD_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_PREPARED_LAYERS: u32 = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplacedLayersWire {
    Reclaimed,
    Leased,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SquashedBlockWire {
    squashed_layer_id: String,
    replaced_layer_ids: Vec<String>,
    replaced_layers: ReplacedLayersWire,
    blocked_reasons: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct FaultySessionWire {
    session_id: String,
    class_detail: String,
    lease_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SweptSessionWire {
    session_id: String,
    disposition: SessionDisposition,
    reason: Option<String>,
    class_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SquashWire {
    manifest_version: u64,
    squashed_blocks: Vec<SquashedBlockWire>,
    swept_sessions: Vec<SweptSessionWire>,
    faulty_sessions: Option<Vec<FaultySessionWire>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerWriteWire {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerReadWire {
    path: String,
    content: String,
    start_line: u64,
    num_lines: u64,
    total_lines: u64,
    bytes_read: u64,
    total_bytes: u64,
    next_offset: Option<u64>,
    truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackPartialEvidence {
    pub requested_live_sessions: u32,
    pub observed_migrated_sessions: u32,
    pub observed_non_migrated_sessions: u32,
    pub dispositions: SessionDispositionCounts,
    pub manifest_version: u64,
    pub squashed_block_count: u32,
    pub replaced_layer_count: u32,
    pub source_layer_ids: Vec<String>,
    pub retained_source_layer_ids: Vec<String>,
    pub manifest_reduced: bool,
    pub content_equivalent: bool,
    pub usable_session_count: u32,
}

/// Collector-owned measurements required to finish the LayerStack evidence.
/// No field has a sentinel representation: unavailable counters remain typed
/// evidence and are never converted to zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SquashLayerstackCollectedEvidence {
    pub effective_remount_parallelism: u32,
    pub source_layer_allocations: Vec<SourceLayerAllocation>,
    pub reclaimed_bytes: Availability<u64>,
    pub s0_baseline: StorageSnapshot,
    pub s1_sampled_peak: StorageSnapshot,
    pub s2_post_commit: StorageSnapshot,
    pub s3_settled: StorageSnapshot,
}

impl SquashLayerstackPartialEvidence {
    #[must_use]
    pub fn finalize(
        self,
        collected: SquashLayerstackCollectedEvidence,
    ) -> crate::model::OperationEvidence {
        crate::model::OperationEvidence::SquashLayerstack(Box::new(SquashLayerstackEvidence {
            requested_live_sessions: self.requested_live_sessions,
            observed_migrated_sessions: self.observed_migrated_sessions,
            observed_non_migrated_sessions: self.observed_non_migrated_sessions,
            dispositions: self.dispositions,
            effective_remount_parallelism: collected.effective_remount_parallelism,
            observed_squashed_block_count: self.squashed_block_count,
            observed_replaced_layer_count: self.replaced_layer_count,
            source_layer_ids: self.source_layer_ids,
            retained_source_layer_ids: self.retained_source_layer_ids,
            source_layer_allocations: collected.source_layer_allocations,
            reclaimed_bytes: collected.reclaimed_bytes,
            s0_baseline: collected.s0_baseline,
            s1_sampled_peak: collected.s1_sampled_peak,
            s2_post_commit: collected.s2_post_commit,
            s3_settled: collected.s3_settled,
            manifest_reduced: self.manifest_reduced,
            content_equivalent: self.content_equivalent,
            usable_session_count: self.usable_session_count,
        }))
    }
}

#[derive(Debug)]
pub struct SquashLayerstackRuntime;

#[derive(Debug)]
pub struct PreparedSquashLayerstack {
    paths: Vec<ProductPath>,
    contents: Vec<String>,
    sessions: SessionRegistry,
    session_baseline: usize,
    observed: Mutex<Option<SquashLayerstackPartialEvidence>>,
}

impl PreparedSquashLayerstack {
    pub fn partial_evidence(&self) -> Result<SquashLayerstackPartialEvidence, ExecutorError> {
        self.observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
            .clone()
            .ok_or(ExecutorError::EvidenceUnavailable {
                operation: OperationId::SquashLayerstack,
                reason: "post-squash response verification did not complete",
            })
    }
}

#[derive(Debug, Clone)]
pub struct SquashLayerstackInvocation {
    request_id: String,
}

impl RuntimeInvocation for SquashLayerstackInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct SquashLayerstackOutput {
    pub metadata: ResponseMetadata,
    response: SquashWire,
}

impl RuntimeOutput for SquashLayerstackOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

fn parse_squash(value: Value) -> Result<(SquashWire, ResponseMetadata), ExecutorError> {
    let metadata = response_metadata(&value, ProductOutputStatus::Succeeded)?;
    let wire = serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
        operation: OperationId::SquashLayerstack,
        detail: "squash_layerstacks output",
    })?;
    Ok((wire, metadata))
}

fn layer_payload(
    context: &RuntimeContext,
    index: u32,
    bytes: u64,
) -> Result<String, ExecutorError> {
    let bytes = usize::try_from(bytes).map_err(|_| ExecutorError::InvalidFixture {
        operation: OperationId::SquashLayerstack,
        reason: "layer payload size does not fit the host",
    })?;
    let seed = format!("layer:{}:{index}", context.fixture_key());
    let digest = format!("{:x}", Sha256::digest(seed.as_bytes()));
    Ok(digest.bytes().cycle().take(bytes).map(char::from).collect())
}

async fn prepare_layer(
    context: &RuntimeContext,
    path: &ProductPath,
    content: &str,
    index: u32,
) -> Result<(), ExecutorError> {
    let value = context
        .gateway()
        .file_write(
            context.sandbox_id(),
            None,
            path,
            content,
            &context.correlation(format!("prepare-layerstack-layer-{index}"))?,
        )
        .await?;
    let wire: LayerWriteWire =
        serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
            operation: OperationId::SquashLayerstack,
            detail: "layer fixture write output",
        })?;
    if wire.kind != "create"
        || wire.path != path.as_str()
        || wire.bytes_written != u64::try_from(content.len()).unwrap_or(u64::MAX)
    {
        return Err(ExecutorError::ResponseSchema {
            operation: OperationId::SquashLayerstack,
            detail: "layer fixture write values",
        });
    }
    Ok(())
}

async fn create_registered_session(
    context: &RuntimeContext,
    sessions: &SessionRegistry,
    index: u32,
) -> Result<CreatedSession, ExecutorError> {
    let session = context
        .workspace_sessions()
        .create_no_op(
            context.sandbox_id().clone(),
            AllowedNetworkProfile::Shared,
            context.correlation(format!("prepare-layerstack-session-{index}"))?,
        )
        .await?;
    if let Err(register_error) = register_session(sessions, session.clone()) {
        let (sandbox_id, session_id) = session.into_parts();
        let cleanup = context
            .workspace_sessions()
            .destroy(
                sandbox_id,
                session_id,
                context.correlation(format!("rollback-layerstack-session-{index}"))?,
            )
            .await;
        return match cleanup {
            Ok(()) => Err(register_error),
            Err(cleanup_error) => Err(cleanup_error.into()),
        };
    }
    Ok(session)
}

async fn prepare_session_activity(
    context: &RuntimeContext,
    session: &CreatedSession,
    index: usize,
) -> Result<(), ExecutorError> {
    let path = ProductPath::new(format!(
        ".eos-benchmark/{}/session-activity-{index}.txt",
        context.fixture_key()
    ))?;
    let content = format!("active:{}:{index}", context.fixture_key());
    let value = context
        .gateway()
        .file_write(
            context.sandbox_id(),
            Some(session.workspace_session_id()),
            &path,
            &content,
            &context.correlation(format!("prepare-layerstack-activity-{index}"))?,
        )
        .await?;
    let wire: LayerWriteWire =
        serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
            operation: OperationId::SquashLayerstack,
            detail: "session activity write output",
        })?;
    if wire.kind != "create"
        || wire.path != path.as_str()
        || wire.bytes_written != u64::try_from(content.len()).unwrap_or(u64::MAX)
    {
        return Err(ExecutorError::ResponseSchema {
            operation: OperationId::SquashLayerstack,
            detail: "session activity write values",
        });
    }
    Ok(())
}

async fn prepare_topology(
    context: &RuntimeContext,
    cell: &SquashLayerstackCell,
    sessions: &SessionRegistry,
) -> Result<(Vec<ProductPath>, Vec<String>), ExecutorError> {
    let eligible = requested_eligible_count(cell.live_sessions, cell.requested_migration_ratio);
    let boundary_sessions = cell.squashable_blocks.saturating_sub(1);
    if eligible < boundary_sessions {
        return Err(ExecutorError::InvalidFixture {
            operation: OperationId::SquashLayerstack,
            reason: "requested eligible sessions cannot form the configured block boundaries",
        });
    }

    let mut session_index = 0_u32;
    for _ in 0..cell.live_sessions.saturating_sub(eligible) {
        create_registered_session(context, sessions, session_index).await?;
        session_index = session_index.saturating_add(1);
    }

    let payload_layer_count = cell
        .squashable_blocks
        .checked_mul(cell.layers_per_block)
        .ok_or(ExecutorError::InvalidFixture {
            operation: OperationId::SquashLayerstack,
            reason: "layer topology size overflows",
        })?;
    let mut paths = Vec::with_capacity(usize::try_from(payload_layer_count).unwrap_or(0));
    let mut contents = Vec::with_capacity(paths.capacity());
    let mut publication_index = 0_u32;
    let mut payload_index = 0_u32;
    for block_index in 0..cell.squashable_blocks {
        for layer_index in 0..cell.layers_per_block {
            let path = ProductPath::new(format!(
                ".eos-benchmark/{}/block-{block_index:04}-layer-{layer_index:04}.txt",
                context.fixture_key()
            ))?;
            let content = layer_payload(context, payload_index, cell.payload_bytes)?;
            prepare_layer(context, &path, &content, publication_index).await?;
            paths.push(path);
            contents.push(content);
            payload_index = payload_index.saturating_add(1);
            publication_index = publication_index.saturating_add(1);
        }

        if block_index.saturating_add(1) < cell.squashable_blocks {
            let marker = ProductPath::new(format!(
                ".eos-benchmark/{}/boundary-{block_index:04}.txt",
                context.fixture_key()
            ))?;
            let marker_content = format!("boundary:{}:{block_index}", context.fixture_key());
            prepare_layer(context, &marker, &marker_content, publication_index).await?;
            publication_index = publication_index.saturating_add(1);
            create_registered_session(context, sessions, session_index).await?;
            session_index = session_index.saturating_add(1);
        }
    }

    let remaining_eligible = eligible.saturating_sub(boundary_sessions);
    if remaining_eligible > 0 {
        let marker = ProductPath::new(format!(
            ".eos-benchmark/{}/boundary-top.txt",
            context.fixture_key()
        ))?;
        let marker_content = format!("boundary:{}:top", context.fixture_key());
        prepare_layer(context, &marker, &marker_content, publication_index).await?;
        for _ in 0..remaining_eligible {
            create_registered_session(context, sessions, session_index).await?;
            session_index = session_index.saturating_add(1);
        }
    }

    if session_index != cell.live_sessions {
        return Err(ExecutorError::InvalidFixture {
            operation: OperationId::SquashLayerstack,
            reason: "prepared session count does not match N",
        });
    }
    if cell.session_activity == SessionActivity::Active {
        for (index, session) in registered_sessions(sessions)?.iter().enumerate() {
            prepare_session_activity(context, session, index).await?;
        }
    }
    Ok((paths, contents))
}

async fn verify_layer_content(
    context: &RuntimeContext,
    path: &ProductPath,
    content: &str,
    index: usize,
) -> Result<bool, ExecutorError> {
    let value = context
        .gateway()
        .file_read(
            context.sandbox_id(),
            None,
            path,
            1,
            1,
            &context.correlation(format!("verify-layerstack-content-{index}"))?,
        )
        .await?;
    let wire: LayerReadWire =
        serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
            operation: OperationId::SquashLayerstack,
            detail: "layer content read output",
        })?;
    let bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
    Ok(wire.path == path.as_str()
        && wire.content == content
        && wire.start_line == 1
        && wire.num_lines == 1
        && wire.total_lines == 1
        && wire.bytes_read == bytes
        && wire.total_bytes == bytes
        && wire.next_offset.is_none()
        && !wire.truncated)
}

#[derive(Debug)]
struct DispositionObservation {
    valid: bool,
    counts: SessionDispositionCounts,
    by_session: BTreeMap<String, SessionDisposition>,
    actual: String,
}

fn observe_dispositions(
    response: &SquashWire,
    expected_session_ids: &BTreeSet<String>,
) -> DispositionObservation {
    let mut valid = true;
    let mut counts = SessionDispositionCounts {
        migrated: 0,
        identity: 0,
        leased: 0,
        faulty: 0,
        session_gone: 0,
    };
    let mut by_session = BTreeMap::new();
    let mut faulty_details = BTreeMap::new();
    for session in &response.swept_sessions {
        let fields_valid = !session.session_id.is_empty()
            && match session.disposition {
                SessionDisposition::Migrated
                | SessionDisposition::Identity
                | SessionDisposition::SessionGone => {
                    session.reason.is_none() && session.class_detail.is_none()
                }
                SessionDisposition::Leased => {
                    session
                        .reason
                        .as_ref()
                        .is_some_and(|reason| !reason.is_empty())
                        && session.class_detail.is_none()
                }
                SessionDisposition::Faulty => {
                    session.reason.is_none()
                        && session
                            .class_detail
                            .as_ref()
                            .is_some_and(|detail| !detail.is_empty())
                }
            };
        valid &= fields_valid;
        if by_session
            .insert(session.session_id.clone(), session.disposition)
            .is_some()
        {
            valid = false;
        }
        match session.disposition {
            SessionDisposition::Migrated => counts.migrated = counts.migrated.saturating_add(1),
            SessionDisposition::Identity => counts.identity = counts.identity.saturating_add(1),
            SessionDisposition::Leased => counts.leased = counts.leased.saturating_add(1),
            SessionDisposition::Faulty => {
                counts.faulty = counts.faulty.saturating_add(1);
                if let Some(detail) = &session.class_detail {
                    faulty_details.insert(session.session_id.clone(), detail.clone());
                }
            }
            SessionDisposition::SessionGone => {
                counts.session_gone = counts.session_gone.saturating_add(1);
            }
        }
    }
    valid &= by_session.keys().cloned().collect::<BTreeSet<_>>() == *expected_session_ids;

    let mut response_faulty = BTreeMap::new();
    if let Some(faulty_sessions) = &response.faulty_sessions {
        valid &= !faulty_sessions.is_empty();
        for faulty in faulty_sessions {
            let entry_valid = !faulty.session_id.is_empty()
                && !faulty.class_detail.is_empty()
                && faulty.lease_errors.iter().all(|error| !error.is_empty());
            valid &= entry_valid;
            if response_faulty
                .insert(faulty.session_id.clone(), faulty.class_detail.clone())
                .is_some()
            {
                valid = false;
            }
        }
    }
    valid &= response_faulty == faulty_details;

    DispositionObservation {
        valid,
        counts,
        actual: format!(
            "requested={},accounted={},migrated={},identity={},leased={},faulty={},session_gone={}",
            expected_session_ids.len(),
            counts.total(),
            counts.migrated,
            counts.identity,
            counts.leased,
            counts.faulty,
            counts.session_gone
        ),
        by_session,
    }
}

fn observe_manifest_reduction(
    response: &SquashWire,
    cell: &SquashLayerstackCell,
) -> (bool, String, u32) {
    let expected_blocks = usize::try_from(cell.squashable_blocks).unwrap_or(usize::MAX);
    let expected_per_block = usize::try_from(cell.layers_per_block).unwrap_or(usize::MAX);
    let expected_replaced = cell.squashable_blocks.saturating_mul(cell.layers_per_block);
    let mut squashed_ids = BTreeSet::new();
    let mut replaced_ids = BTreeSet::new();
    let mut valid_blocks = response.squashed_blocks.len() == expected_blocks;
    for block in &response.squashed_blocks {
        let disposition_valid = match block.replaced_layers {
            ReplacedLayersWire::Reclaimed => block.blocked_reasons.is_none(),
            ReplacedLayersWire::Leased => block.blocked_reasons.as_ref().is_some_and(|reasons| {
                !reasons.is_empty() && reasons.iter().all(|reason| !reason.is_empty())
            }),
        };
        valid_blocks &= disposition_valid
            && !block.squashed_layer_id.is_empty()
            && squashed_ids.insert(block.squashed_layer_id.as_str())
            && block.replaced_layer_ids.len() == expected_per_block;
        for replaced_id in &block.replaced_layer_ids {
            valid_blocks &= !replaced_id.is_empty() && replaced_ids.insert(replaced_id.as_str());
        }
    }
    valid_blocks &= squashed_ids.is_disjoint(&replaced_ids);
    let replaced_layer_count = u32::try_from(replaced_ids.len()).unwrap_or(u32::MAX);
    let valid =
        response.manifest_version > 0 && valid_blocks && replaced_layer_count == expected_replaced;
    (
        valid,
        format!(
            "manifest={},blocks={},replaced={},reclaimed_blocks={},leased_blocks={}",
            response.manifest_version,
            response.squashed_blocks.len(),
            replaced_layer_count,
            response
                .squashed_blocks
                .iter()
                .filter(|block| block.replaced_layers == ReplacedLayersWire::Reclaimed)
                .count(),
            response
                .squashed_blocks
                .iter()
                .filter(|block| block.replaced_layers == ReplacedLayersWire::Leased)
                .count()
        ),
        replaced_layer_count,
    )
}

fn retire_product_destroyed_sessions(
    context: &RuntimeContext,
    sessions: &SessionRegistry,
    dispositions: &BTreeMap<String, SessionDisposition>,
) -> Result<(), ExecutorError> {
    let retired: BTreeSet<String> = dispositions
        .iter()
        .filter(|&(_, disposition)| {
            matches!(
                disposition,
                SessionDisposition::Faulty | SessionDisposition::SessionGone
            )
        })
        .map(|(session_id, _)| session_id.clone())
        .collect();
    if retired.is_empty() {
        return Ok(());
    }
    let owned = registered_sessions(sessions)?;
    for session in owned
        .iter()
        .filter(|session| retired.contains(session.workspace_session_id().as_str()))
    {
        context
            .workspace_sessions()
            .retire_product_destroyed(context.sandbox_id(), session.workspace_session_id())?;
    }
    sessions
        .lock()
        .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
        .retain(|session| !retired.contains(session.workspace_session_id().as_str()));
    Ok(())
}

impl OperationLifecycle for SquashLayerstackRuntime {
    type Cell = SquashLayerstackCell;
    type Prepared = PreparedSquashLayerstack;
    type Invocation = SquashLayerstackInvocation;
    type Output = SquashLayerstackOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.remount_parallelism != context.gateway_remount_parallelism() {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::SquashLayerstack,
                reason: "cell remount parallelism does not match the effective gateway block",
            });
        }
        if !cell.requested_migration_ratio.is_valid()
            || cell.squashable_blocks == 0
            || cell.layers_per_block == 0
            || cell.layers_per_block < 2
            || cell
                .squashable_blocks
                .checked_mul(cell.layers_per_block)
                .is_none_or(|layers| layers > MAX_PREPARED_LAYERS)
            || cell.payload_bytes == 0
            || cell.payload_bytes > MAX_LAYER_PAYLOAD_BYTES
        {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::SquashLayerstack,
                reason: "layer topology factors exceed the bounded product contract",
            });
        }
        let sessions = session_registry();
        let session_baseline = context.workspace_sessions().owned_session_count()?;
        let (paths, contents) = match prepare_topology(context, cell, &sessions).await {
            Ok(topology) => topology,
            Err(error) => {
                let cleanup =
                    teardown_registered_sessions(context, sessions.clone(), session_baseline).await;
                if cleanup.baseline_restored {
                    return Err(error);
                }
                return Err(ExecutorError::DispatchedInvocationFailure {
                    operation: OperationId::SquashLayerstack,
                    detail: format!(
                        "topology preparation failed ({error}); cleanup destroyed {}/{} sessions with {} errors",
                        cleanup.destroyed_sessions,
                        cleanup.expected_destroyed_sessions,
                        cleanup.errors.len()
                    ),
                });
            }
        };
        Ok(PreparedSquashLayerstack {
            paths,
            contents,
            sessions,
            session_baseline,
            observed: Mutex::new(None),
        })
    }

    fn invocations(
        _prepared: &Self::Prepared,
        _cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        Ok(vec![SquashLayerstackInvocation {
            request_id: "squash-layerstack-0".to_owned(),
        }])
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let value = context
                .gateway()
                .squash_layerstacks(context.sandbox_id(), &context.correlation(&request_id)?)
                .await?;
            let (response, metadata) = parse_squash(value)?;
            Ok(SquashLayerstackOutput { metadata, response })
        }
        .await;
        match result {
            Ok(output) => InvocationOutcome::Succeeded { request_id, output },
            Err(error) => InvocationOutcome::Failed { request_id, error },
        }
    }

    async fn verify(
        context: &RuntimeContext,
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
    ) -> Result<Verification, ExecutorError> {
        if outcomes.len() != 1 {
            return Err(ExecutorError::InvocationCount {
                operation: OperationId::SquashLayerstack,
                expected: 1,
                actual: outcomes.len(),
            });
        }
        let output = outcomes[0].output();
        let response = output.map(|output| &output.response);
        let mut checks = Vec::with_capacity(4);
        let prepared_sessions = registered_sessions(&prepared.sessions)?;
        let expected_session_ids: BTreeSet<String> = prepared_sessions
            .iter()
            .map(|session| session.workspace_session_id().as_str().to_owned())
            .collect();
        if expected_session_ids.len() != usize::try_from(cell.live_sessions).unwrap_or(usize::MAX) {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::SquashLayerstack,
                reason: "owned prepared session set does not match N",
            });
        }

        let started = Instant::now();
        let mut content_equivalent = true;
        for (index, (path, content)) in prepared.paths.iter().zip(&prepared.contents).enumerate() {
            content_equivalent &= verify_layer_content(context, path, content, index).await?;
        }
        checks.push(check_result(
            context,
            OperationId::SquashLayerstack,
            CheckId::LayerstackContentEquivalence,
            None,
            output.is_some() && content_equivalent,
            format!("equivalent_layers={}", prepared.paths.len()),
            format!("equivalent={content_equivalent}"),
            started,
        ));

        let started = Instant::now();
        let (manifest_reduced, manifest_actual, replaced_layer_count) = match response {
            Some(response) => observe_manifest_reduction(response, cell),
            None => (
                false,
                outcomes[0]
                    .error()
                    .map_or_else(|| "missing output".to_owned(), ToString::to_string),
                0,
            ),
        };
        checks.push(check_result(
            context,
            OperationId::SquashLayerstack,
            CheckId::LayerstackManifestReduction,
            None,
            manifest_reduced,
            format!(
                "blocks={},replaced={}",
                cell.squashable_blocks,
                cell.squashable_blocks.saturating_mul(cell.layers_per_block)
            ),
            manifest_actual,
            started,
        ));

        let started = Instant::now();
        let disposition_observation =
            response.map(|response| observe_dispositions(response, &expected_session_ids));
        let disposition_valid = disposition_observation.as_ref().is_some_and(|observation| {
            observation.valid && observation.counts.total() == cell.live_sessions
        });
        checks.push(check_result(
            context,
            OperationId::SquashLayerstack,
            CheckId::LayerstackDispositionAccounting,
            None,
            disposition_valid,
            format!(
                "requested={},accounted={}",
                cell.live_sessions, cell.live_sessions
            ),
            disposition_observation.as_ref().map_or_else(
                || {
                    outcomes[0]
                        .error()
                        .map_or_else(|| "missing output".to_owned(), ToString::to_string)
                },
                |observation| observation.actual.clone(),
            ),
            started,
        ));

        let started = Instant::now();
        let mut usable_session_count = 0_u32;
        let mut expected_usable_count = 0_u32;
        let mut usability_failures = Vec::new();
        if let Some(observation) = &disposition_observation {
            for (index, session) in prepared_sessions.iter().enumerate() {
                let Some(disposition) = observation
                    .by_session
                    .get(session.workspace_session_id().as_str())
                else {
                    usability_failures.push(format!(
                        "{}:missing_disposition",
                        session.workspace_session_id().as_str()
                    ));
                    continue;
                };
                if matches!(
                    disposition,
                    SessionDisposition::Migrated
                        | SessionDisposition::Identity
                        | SessionDisposition::Leased
                ) {
                    expected_usable_count = expected_usable_count.saturating_add(1);
                    match probe_session(
                        context,
                        session.workspace_session_id(),
                        &format!("verify-layerstack-session-{index}"),
                    )
                    .await
                    {
                        Ok(()) => usable_session_count = usable_session_count.saturating_add(1),
                        Err(error) => usability_failures.push(format!(
                            "{}:{error}",
                            session.workspace_session_id().as_str()
                        )),
                    }
                }
            }
        }
        let usability_valid = disposition_valid
            && usability_failures.is_empty()
            && usable_session_count == expected_usable_count;
        checks.push(check_result(
            context,
            OperationId::SquashLayerstack,
            CheckId::LayerstackSessionUsability,
            None,
            usability_valid,
            format!("expected_usable={expected_usable_count}"),
            format!(
                "usable={usable_session_count},failures={}",
                usability_failures.join("|")
            ),
            started,
        ));

        if let Some(observation) = &disposition_observation {
            if observation.valid {
                retire_product_destroyed_sessions(
                    context,
                    &prepared.sessions,
                    &observation.by_session,
                )?;
            }
        }

        // A failed product invocation has no manifest or topology observation.
        // Do not turn that absence into zero-valued evidence; the failed
        // outcome remains authoritative and partial evidence stays unavailable.
        if let Some(response) = response {
            let counts = disposition_observation.as_ref().map_or(
                SessionDispositionCounts {
                    migrated: 0,
                    identity: 0,
                    leased: 0,
                    faulty: 0,
                    session_gone: 0,
                },
                |observation| observation.counts,
            );
            let source_layer_ids = response
                .squashed_blocks
                .iter()
                .flat_map(|block| block.replaced_layer_ids.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let retained_source_layer_ids = response
                .squashed_blocks
                .iter()
                .filter(|block| block.replaced_layers == ReplacedLayersWire::Leased)
                .flat_map(|block| block.replaced_layer_ids.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let partial = SquashLayerstackPartialEvidence {
                requested_live_sessions: cell.live_sessions,
                observed_migrated_sessions: counts.migrated,
                observed_non_migrated_sessions: counts.non_migrated(),
                dispositions: counts,
                manifest_version: response.manifest_version,
                squashed_block_count: u32::try_from(response.squashed_blocks.len())
                    .unwrap_or(u32::MAX),
                replaced_layer_count,
                source_layer_ids,
                retained_source_layer_ids,
                manifest_reduced,
                content_equivalent,
                usable_session_count,
            };
            *prepared
                .observed
                .lock()
                .map_err(|_| ExecutorError::SessionRegistryUnavailable)? = Some(partial);
        }
        Ok(Verification { checks })
    }

    async fn teardown(context: &RuntimeContext, prepared: &mut Self::Prepared) -> TeardownResult {
        teardown_registered_sessions(
            context,
            prepared.sessions.clone(),
            prepared.session_baseline,
        )
        .await
    }

    fn evidence(
        prepared: &Self::Prepared,
        _cell: &Self::Cell,
        _outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<crate::model::OperationEvidence, ExecutorError> {
        let _ = prepared.partial_evidence()?;
        Err(ExecutorError::EvidenceUnavailable {
            operation: OperationId::SquashLayerstack,
            reason: "scheduler storage snapshots and product trace phase durations are required",
        })
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn strict_squash_wire_rejects_unknown_fields() {
        let value = serde_json::json!({
            "manifest_version": 9,
            "squashed_blocks": [],
            "swept_sessions": [],
            "unexpected": true
        });
        assert!(parse_squash(value).is_err());
    }

    #[test]
    fn strict_squash_wire_requires_swept_session_accounting() {
        let value = serde_json::json!({
            "manifest_version": 9,
            "squashed_blocks": [],
            "faulty_sessions": null
        });
        assert!(parse_squash(value).is_err());
    }

    #[test]
    fn strict_squash_wire_accepts_product_optional_field_omission() {
        let value = serde_json::json!({
            "manifest_version": 9,
            "squashed_blocks": [{
                "squashed_layer_id": "squashed-1",
                "replaced_layer_ids": ["layer-1", "layer-2"],
                "replaced_layers": "reclaimed"
            }],
            "swept_sessions": [{
                "session_id": "session-1",
                "disposition": "migrated"
            }]
        });
        let (wire, metadata) = parse_squash(value).expect("current squash response");

        assert_eq!(wire.manifest_version, 9);
        assert_eq!(wire.squashed_blocks.len(), 1);
        assert_eq!(wire.swept_sessions.len(), 1);
        assert!(wire.faulty_sessions.is_none());
        assert_eq!(metadata.status, ProductOutputStatus::Succeeded);
        assert!(metadata.response_bytes > 0);
    }

    #[test]
    fn disposition_totals_are_explicit() {
        let counts = SessionDispositionCounts {
            migrated: 1,
            identity: 2,
            leased: 3,
            faulty: 4,
            session_gone: 5,
        };
        assert_eq!(counts.total(), 15);
        assert_eq!(counts.non_migrated(), 14);
    }

    #[test]
    fn requested_eligible_count_rounds_halves_up_and_stays_bounded() {
        assert_eq!(requested_eligible_count(20, UnitRatio(0.0)), 0);
        assert_eq!(requested_eligible_count(1, UnitRatio(0.5)), 1);
        assert_eq!(requested_eligible_count(5, UnitRatio(0.5)), 3);
        assert_eq!(requested_eligible_count(20, UnitRatio(0.5)), 10);
        assert_eq!(requested_eligible_count(20, UnitRatio(1.0)), 20);
    }

    #[test]
    fn validation_rejects_blocks_without_enough_eligible_boundaries() {
        fn controlled<T>(value: T) -> Factor<T> {
            Factor {
                role: crate::model::FactorRole::Controlled,
                values: vec![value],
                control: None,
            }
        }

        let plan = SquashLayerstackPlan {
            enabled: true,
            factors: SquashLayerstackFactors {
                live_sessions: controlled(1),
                requested_migration_ratio: controlled(UnitRatio(0.0)),
                remount_parallelism: controlled(4),
                squashable_blocks: controlled(2),
                layers_per_block: controlled(2),
                payload_bytes: controlled(4_096),
                session_activity: controlled(SessionActivity::Idle),
            },
        };

        assert!(validate(&plan).contains(&OperationValidationError {
            operation: OperationId::SquashLayerstack,
            factor: FactorId::SquashableBlocks,
            violation: FactorViolation::IncompatibleCombination,
        }));
    }

    #[test]
    fn manifest_observation_requires_exact_blocks_and_unique_layers() {
        let cell = SquashLayerstackCell {
            live_sessions: 2,
            requested_migration_ratio: UnitRatio(1.0),
            remount_parallelism: 4,
            squashable_blocks: 2,
            layers_per_block: 2,
            payload_bytes: 4_096,
            session_activity: SessionActivity::Idle,
            resolved_isolation: ResolvedIsolationPolicy::FreshTopologyPerTrial,
        };
        let (response, _) = parse_squash(serde_json::json!({
            "manifest_version": 9,
            "squashed_blocks": [
                {
                    "squashed_layer_id": "squashed-1",
                    "replaced_layer_ids": ["layer-1", "layer-2"],
                    "replaced_layers": "reclaimed",
                    "blocked_reasons": null
                },
                {
                    "squashed_layer_id": "squashed-2",
                    "replaced_layer_ids": ["layer-3", "layer-4"],
                    "replaced_layers": "leased",
                    "blocked_reasons": ["workspace-session:session-2"]
                }
            ],
            "swept_sessions": [],
            "faulty_sessions": null
        }))
        .expect("strict response");

        let (valid, actual, replaced) = observe_manifest_reduction(&response, &cell);
        assert!(valid, "{actual}");
        assert_eq!(replaced, 4);

        let mut duplicate = response;
        duplicate.squashed_blocks[1].replaced_layer_ids[0] = "layer-2".to_owned();
        assert!(!observe_manifest_reduction(&duplicate, &cell).0);
    }

    #[test]
    fn disposition_observation_requires_exact_ids_and_fault_consistency() {
        let expected = BTreeSet::from(["session-1".to_owned(), "session-2".to_owned()]);
        let (response, _) = parse_squash(serde_json::json!({
            "manifest_version": 9,
            "squashed_blocks": [],
            "swept_sessions": [
                {
                    "session_id": "session-1",
                    "disposition": "migrated",
                    "reason": null,
                    "class_detail": null
                },
                {
                    "session_id": "session-2",
                    "disposition": "faulty",
                    "reason": null,
                    "class_detail": "remount_failed"
                }
            ],
            "faulty_sessions": [{
                "session_id": "session-2",
                "class_detail": "remount_failed",
                "lease_errors": ["mount rejected"]
            }]
        }))
        .expect("strict response");

        let observation = observe_dispositions(&response, &expected);
        assert!(observation.valid, "{}", observation.actual);
        assert_eq!(observation.counts.migrated, 1);
        assert_eq!(observation.counts.faulty, 1);

        let mut mismatch = response;
        mismatch.faulty_sessions.as_mut().expect("faulty sessions")[0].class_detail =
            "different".to_owned();
        assert!(!observe_dispositions(&mismatch, &expected).valid);
    }

    #[test]
    fn collector_finalization_preserves_partial_and_measured_values() {
        fn available<T>(value: T) -> Availability<T> {
            Availability::Available { value }
        }

        #[allow(clippy::too_many_arguments)]
        fn snapshot(
            monotonic_offset_ns: u64,
            sampled: bool,
            manifest_version: u64,
            root_hash: &str,
            active_layer_count: u64,
            active_lease_count: u64,
            active_logical_bytes: u64,
            active_allocated_bytes: u64,
            storage_logical_bytes: u64,
            storage_allocated_bytes: u64,
            staging_entry_count: u64,
        ) -> StorageSnapshot {
            StorageSnapshot {
                monotonic_offset_ns: available(monotonic_offset_ns),
                sampled,
                manifest_version: available(manifest_version),
                root_hash: available(root_hash.to_owned()),
                active_layer_count: available(active_layer_count),
                active_lease_count: available(active_lease_count),
                active_logical_bytes: available(active_logical_bytes),
                active_allocated_bytes: available(active_allocated_bytes),
                storage_logical_bytes: available(storage_logical_bytes),
                storage_allocated_bytes: available(storage_allocated_bytes),
                staging_entry_count: available(staging_entry_count),
            }
        }

        let source_layer_ids = (1..=8)
            .map(|index| format!("layer-{index}"))
            .collect::<Vec<_>>();
        let retained_source_layer_ids = vec!["layer-8".to_owned()];
        let partial = SquashLayerstackPartialEvidence {
            requested_live_sessions: 3,
            observed_migrated_sessions: 2,
            observed_non_migrated_sessions: 1,
            dispositions: SessionDispositionCounts {
                migrated: 2,
                identity: 1,
                leased: 0,
                faulty: 0,
                session_gone: 0,
            },
            manifest_version: 9,
            squashed_block_count: 1,
            replaced_layer_count: 8,
            source_layer_ids: source_layer_ids.clone(),
            retained_source_layer_ids: retained_source_layer_ids.clone(),
            manifest_reduced: true,
            content_equivalent: true,
            usable_session_count: 3,
        };
        let source_layer_allocations = source_layer_ids
            .iter()
            .map(|layer_id| SourceLayerAllocation {
                layer_id: layer_id.clone(),
                logical_bytes: available(10),
                allocated_bytes: available(12),
            })
            .collect::<Vec<_>>();
        let s0_baseline = snapshot(100, false, 8, "root-before", 8, 1, 80, 96, 80, 96, 0);
        let s1_sampled_peak = snapshot(125, true, 8, "root-before", 8, 1, 80, 96, 90, 120, 1);
        let s2_post_commit = snapshot(150, false, 9, "root-after", 1, 1, 10, 12, 20, 30, 1);
        let s3_settled = snapshot(200, false, 9, "root-after", 1, 1, 10, 12, 10, 12, 0);
        let evidence = partial.finalize(SquashLayerstackCollectedEvidence {
            effective_remount_parallelism: 2,
            source_layer_allocations: source_layer_allocations.clone(),
            reclaimed_bytes: available(84),
            s0_baseline: s0_baseline.clone(),
            s1_sampled_peak: s1_sampled_peak.clone(),
            s2_post_commit: s2_post_commit.clone(),
            s3_settled: s3_settled.clone(),
        });

        let crate::model::OperationEvidence::SquashLayerstack(evidence) = evidence else {
            panic!("typed layerstack evidence");
        };
        assert_eq!(evidence.requested_live_sessions, 3);
        assert_eq!(evidence.observed_migrated_sessions, 2);
        assert_eq!(evidence.effective_remount_parallelism, 2);
        assert_eq!(evidence.source_layer_ids, source_layer_ids);
        assert_eq!(
            evidence.retained_source_layer_ids,
            retained_source_layer_ids
        );
        assert_eq!(evidence.source_layer_allocations, source_layer_allocations);
        assert_eq!(evidence.reclaimed_bytes, available(84));
        assert_eq!(evidence.s0_baseline, s0_baseline);
        assert_eq!(evidence.s1_sampled_peak, s1_sampled_peak);
        assert_eq!(evidence.s2_post_commit, s2_post_commit);
        assert_eq!(evidence.s3_settled, s3_settled);
        assert!(evidence.manifest_reduced);
        assert!(evidence.content_equivalent);
    }

    #[test]
    fn layerstack_evidence_roundtrip_preserves_unavailable_counters() {
        let unavailable = Availability::Unavailable {
            source: "product_query".to_owned(),
            reason: "allocated_bytes_not_reported".to_owned(),
        };
        let snapshot = StorageSnapshot {
            monotonic_offset_ns: Availability::Available { value: 10 },
            sampled: false,
            manifest_version: Availability::Available { value: 9 },
            root_hash: Availability::Available {
                value: "root-after".to_owned(),
            },
            active_layer_count: Availability::Available { value: 1 },
            active_lease_count: Availability::Available { value: 0 },
            active_logical_bytes: Availability::Available { value: 10 },
            active_allocated_bytes: unavailable.clone(),
            storage_logical_bytes: Availability::Available { value: 10 },
            storage_allocated_bytes: unavailable.clone(),
            staging_entry_count: Availability::Available { value: 0 },
        };
        let evidence = SquashLayerstackEvidence {
            requested_live_sessions: 0,
            observed_migrated_sessions: 0,
            observed_non_migrated_sessions: 0,
            dispositions: SessionDispositionCounts {
                migrated: 0,
                identity: 0,
                leased: 0,
                faulty: 0,
                session_gone: 0,
            },
            effective_remount_parallelism: 0,
            observed_squashed_block_count: 1,
            observed_replaced_layer_count: 1,
            source_layer_ids: vec!["layer-1".to_owned()],
            retained_source_layer_ids: Vec::new(),
            source_layer_allocations: vec![SourceLayerAllocation {
                layer_id: "layer-1".to_owned(),
                logical_bytes: Availability::Available { value: 10 },
                allocated_bytes: unavailable.clone(),
            }],
            reclaimed_bytes: unavailable.clone(),
            s0_baseline: snapshot.clone(),
            s1_sampled_peak: snapshot.clone(),
            s2_post_commit: snapshot.clone(),
            s3_settled: snapshot,
            manifest_reduced: true,
            content_equivalent: true,
            usable_session_count: 0,
        };

        let serialized = serde_json::to_value(&evidence).expect("serialize evidence");
        assert_eq!(
            serialized["reclaimed_bytes"],
            serde_json::json!({
                "availability": "unavailable",
                "source": "product_query",
                "reason": "allocated_bytes_not_reported"
            })
        );
        assert_ne!(
            serialized["reclaimed_bytes"],
            serde_json::json!({"availability": "available", "value": 0})
        );
        let decoded: SquashLayerstackEvidence =
            serde_json::from_value(serialized).expect("deserialize evidence");
        assert_eq!(decoded, evidence);
    }
}
