use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::daemon_session::{CreatedSession, WorkspaceSessionId, WorkspaceSessionLifecycle};

use crate::definitions::{
    CheckReference, ComparisonParticipation, ComparisonProjectionDefinition, FactorConstraint,
    FactorDefinition, FactorUnit, FactorValueKind, OperationDefinition, FACTOR_SCHEMA_REVISION,
    OPERATION_SEMANTIC_REVISION, SUPPORTED_COHORTS,
};
use crate::gateway::{Correlation, ProductEdit, ProductPath};
use crate::model::{
    validate_factor, validate_nonzero_u32, validate_nonzero_u64, validate_unit_ratio, CheckId,
    CleanupPolicy, CountSemantics, ExecutionShape, Factor, FactorId, FamilyId, IsolationPolicy,
    OperationEvidence, OperationId, OperationValidationError, ProductAccess, ProductOperation,
    ResolvedIsolationPolicy, SecurityClass, UnitRatio,
};

use super::{
    check_result, register_session, session_registry, teardown_registered_sessions, ExecutorError,
    InvocationOutcome, OperationLifecycle, ProductOutputStatus, ResponseMetadata, RuntimeContext,
    RuntimeInvocation, RuntimeOutput, SessionRegistry, TeardownResult, Verification,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileReadSource {
    Snapshot,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationDestination {
    Session,
    Publish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetMode {
    Independent,
    SameTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationAttribution {
    WorkspaceSession,
    PublishedOperationLayer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadFactors {
    pub concurrent_requests: Factor<u32>,
    pub returned_bytes: Factor<u64>,
    pub source: Factor<FileReadSource>,
    pub target_mode: Factor<TargetMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadPlan {
    pub enabled: bool,
    pub factors: FileReadFactors,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteFactors {
    pub concurrent_requests: Factor<u32>,
    pub content_bytes: Factor<u64>,
    pub destination: Factor<MutationDestination>,
    pub target_mode: Factor<TargetMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWritePlan {
    pub enabled: bool,
    pub factors: FileWriteFactors,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEditFactors {
    pub concurrent_requests: Factor<u32>,
    pub file_bytes: Factor<u64>,
    pub replacement_count: Factor<u32>,
    pub match_density: Factor<UnitRatio>,
    pub destination: Factor<MutationDestination>,
    pub target_mode: Factor<TargetMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEditPlan {
    pub enabled: bool,
    pub factors: FileEditFactors,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlameFactors {
    pub concurrent_requests: Factor<u32>,
    pub line_count: Factor<u32>,
    pub ownership_segments: Factor<u32>,
    pub auditability_event_count: Factor<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlamePlan {
    pub enabled: bool,
    pub factors: FileBlameFactors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadCell {
    pub concurrent_requests: u32,
    pub returned_bytes: u64,
    pub source: FileReadSource,
    pub target_mode: TargetMode,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteCell {
    pub concurrent_requests: u32,
    pub content_bytes: u64,
    pub destination: MutationDestination,
    pub target_mode: TargetMode,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEditCell {
    pub concurrent_requests: u32,
    pub file_bytes: u64,
    pub replacement_count: u32,
    pub match_density: UnitRatio,
    pub destination: MutationDestination,
    pub target_mode: TargetMode,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlameCell {
    pub concurrent_requests: u32,
    pub line_count: u32,
    pub ownership_segments: u32,
    pub auditability_event_count: u32,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadEvidence {
    pub requested_bytes: u64,
    pub returned_bytes: u64,
    pub returned_lines: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteEvidence {
    pub requested_bytes: u64,
    pub observed_bytes: u64,
    pub expected_sha256: String,
    pub observed_sha256: String,
    pub attribution: MutationAttribution,
    pub attributed_layer_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEditEvidence {
    pub requested_replacements: u32,
    pub applied_replacements: u32,
    pub before_sha256: String,
    pub expected_sha256: String,
    pub observed_sha256: String,
    pub attribution: MutationAttribution,
    pub attributed_layer_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlameEvidence {
    pub requested_lines: u32,
    pub returned_ranges: u32,
    pub covered_lines: u32,
    pub expected_ownership_segments: u32,
    pub matched_ownership_segments: u32,
    pub observed_auditability_events: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadComparisonIdentity {
    pub concurrent_requests: u32,
    pub returned_bytes: u64,
    pub source: FileReadSource,
    pub target_mode: TargetMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteComparisonIdentity {
    pub concurrent_requests: u32,
    pub content_bytes: u64,
    pub destination: MutationDestination,
    pub target_mode: TargetMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEditComparisonIdentity {
    pub concurrent_requests: u32,
    pub file_bytes: u64,
    pub replacement_count: u32,
    pub match_density: UnitRatio,
    pub destination: MutationDestination,
    pub target_mode: TargetMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlameComparisonIdentity {
    pub concurrent_requests: u32,
    pub line_count: u32,
    pub ownership_segments: u32,
    pub auditability_event_count: u32,
}

const CONCURRENT_REQUESTS: FactorDefinition = FactorDefinition {
    id: FactorId::ConcurrentRequests,
    label: "Concurrent requests",
    help: "Independent product requests released together from one trial barrier.",
    value_kind: FactorValueKind::UnsignedInteger,
    unit: Some(FactorUnit::Count),
    constraint: FactorConstraint::Positive,
    comparison: ComparisonParticipation::ScientificInvariant,
};
const TARGET_MODE: FactorDefinition = FactorDefinition {
    id: FactorId::TargetMode,
    label: "Target mode",
    help: "Independent targets preserve the baseline; same-target mode is an explicit contention treatment.",
    value_kind: FactorValueKind::Choice,
    unit: None,
    constraint: FactorConstraint::Choices {
        values: &["independent", "same_target"],
    },
    comparison: ComparisonParticipation::ScientificInvariant,
};
const READ_FACTORS: &[FactorDefinition] = &[
    CONCURRENT_REQUESTS,
    FactorDefinition {
        id: FactorId::ReturnedBytes,
        label: "Returned bytes",
        help: "Requested UTF-8 line-window size, bounded by the fixed product output and line caps.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Bytes),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::ReadSource,
        label: "Read source",
        help: "Read from the published snapshot or from an explicitly prepared live workspace session.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["snapshot", "session"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    TARGET_MODE,
];
const WRITE_FACTORS: &[FactorDefinition] = &[
    CONCURRENT_REQUESTS,
    FactorDefinition {
        id: FactorId::ContentBytes,
        label: "Content bytes",
        help: "Deterministic non-sparse content bytes written by each request.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Bytes),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::MutationDestination,
        label: "Mutation destination",
        help: "Session mutates only live workspace state; publish creates one attributed operation layer.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["session", "publish"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    TARGET_MODE,
];
const EDIT_FACTORS: &[FactorDefinition] = &[
    CONCURRENT_REQUESTS,
    FactorDefinition {
        id: FactorId::FileBytes,
        label: "File bytes",
        help: "Deterministic UTF-8 fixture size before ordered exact-string replacements.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Bytes),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::ReplacementCount,
        label: "Replacement count",
        help: "Number of unique ordered exact-string replacements requested and verified.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::MatchDensity,
        label: "Match density",
        help: "Fraction of deterministic fixture regions containing an exact replacement match.",
        value_kind: FactorValueKind::UnitRatio,
        unit: Some(FactorUnit::Ratio),
        constraint: FactorConstraint::UnitInterval,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::MutationDestination,
        label: "Mutation destination",
        help: "Session mutates only live workspace state; publish creates one attributed operation layer.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["session", "publish"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    TARGET_MODE,
];
const BLAME_FACTORS: &[FactorDefinition] = &[
    CONCURRENT_REQUESTS,
    FactorDefinition {
        id: FactorId::LineCount,
        label: "Line count",
        help: "Deterministic number of published text lines covered by the ownership query.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::OwnershipSegments,
        label: "Ownership segments",
        help: "Expected contiguous per-line ownership segments in the published fixture.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::AuditabilityEventCount,
        label: "Auditability events",
        help: "Deterministic publish-auditability events used to construct ownership history.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
];

const READ_CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::FileReadWindow,
        label: "Read window",
        help: "Returned UTF-8 lines exactly cover the requested bounded window.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
    CheckReference {
        id: CheckId::FileContentHash,
        label: "File content hash",
        help: "Observed file content and size match the deterministic fixture expectation.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
];
const MUTATION_CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::FileContentHash,
        label: "File content hash",
        help: "Observed file content and size match the deterministic mutation expectation.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
    CheckReference {
        id: CheckId::MutationAttribution,
        label: "Mutation attribution",
        help: "The mutation remains session-local or creates exactly one attributed published layer as selected.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
];
const EDIT_CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::FileEditReplacementCount,
        label: "Replacement count",
        help: "The product applied exactly the requested ordered replacement count.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
    CheckReference {
        id: CheckId::FileContentHash,
        label: "File content hash",
        help: "Observed edited content matches the deterministic expected hash.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
    CheckReference {
        id: CheckId::MutationAttribution,
        label: "Mutation attribution",
        help: "The edit remains session-local or creates exactly one attributed published layer as selected.",
        semantic_revision: 1,
        evidence_limit: 16,
    },
];
const BLAME_CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::BlameRangeCoverage,
        label: "Ownership range coverage",
        help: "Returned ownership ranges are gap-free, ordered, and cover every requested line exactly once.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
    CheckReference {
        id: CheckId::BlameOwnership,
        label: "Ownership attribution",
        help: "Every returned range matches the deterministic EphemeralOS publish-auditability owner.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
];

const READ_COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::ConcurrentRequests,
    FactorId::ReturnedBytes,
    FactorId::ReadSource,
    FactorId::TargetMode,
];
const WRITE_COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::ConcurrentRequests,
    FactorId::ContentBytes,
    FactorId::MutationDestination,
    FactorId::TargetMode,
];
const EDIT_COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::ConcurrentRequests,
    FactorId::FileBytes,
    FactorId::ReplacementCount,
    FactorId::MatchDensity,
    FactorId::MutationDestination,
    FactorId::TargetMode,
];
const BLAME_COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::ConcurrentRequests,
    FactorId::LineCount,
    FactorId::OwnershipSegments,
    FactorId::AuditabilityEventCount,
];

pub const READ_DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::FileRead,
    family: FamilyId::Files,
    label: "Read file",
    help: "Reads a bounded UTF-8 line window through the public file_read operation.",
    measured_boundary: "One request reads from a verified published snapshot or prepared live session; fixture preparation and content verification remain outside request latency.",
    count_semantics_help: "Concurrent requests is the number of independent file_read product requests released in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentRequests {
        factor: FactorId::ConcurrentRequests,
    },
    execution_shape: ExecutionShape::BarrierRequestBatch,
    isolation: IsolationPolicy::ReusableVerifiedFixture,
    cleanup: CleanupPolicy::VerifyFixtureUnchanged,
    product_access: ProductAccess::PublicGateway(ProductOperation::FileRead),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::PublicReadOnly,
    factors: READ_FACTORS,
    checks: READ_CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: READ_COMPARISON_FACTORS,
    },
};

pub const WRITE_DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::FileWrite,
    family: FamilyId::Files,
    label: "Write file",
    help: "Overwrites deterministic content in a live session or publishes one attributed operation layer.",
    measured_boundary: "One request overwrites an independent or explicitly contended target; session setup or fresh publish topology and later verification are separately timed.",
    count_semantics_help: "Concurrent requests is the number of independent file_write product requests released in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentRequests {
        factor: FactorId::ConcurrentRequests,
    },
    execution_shape: ExecutionShape::BarrierRequestBatch,
    isolation: IsolationPolicy::MutationDestinationDependent,
    cleanup: CleanupPolicy::ResolveFromIsolation,
    product_access: ProductAccess::PublicGateway(ProductOperation::FileWrite),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::PublicMutation,
    factors: WRITE_FACTORS,
    checks: MUTATION_CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: WRITE_COMPARISON_FACTORS,
    },
};

pub const EDIT_DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::FileEdit,
    family: FamilyId::Files,
    label: "Edit file",
    help: "Applies typed ordered exact-string replacements in a live session or one attributed published layer.",
    measured_boundary: "One request applies the configured ordered replacements; deterministic fixture creation, attribution verification, and cleanup remain separately timed.",
    count_semantics_help: "Concurrent requests is the number of independent file_edit product requests released in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentRequests {
        factor: FactorId::ConcurrentRequests,
    },
    execution_shape: ExecutionShape::BarrierRequestBatch,
    isolation: IsolationPolicy::MutationDestinationDependent,
    cleanup: CleanupPolicy::ResolveFromIsolation,
    product_access: ProductAccess::PublicGateway(ProductOperation::FileEdit),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::PublicMutation,
    factors: EDIT_FACTORS,
    checks: EDIT_CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: EDIT_COMPARISON_FACTORS,
    },
};

pub const BLAME_DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::FileBlame,
    family: FamilyId::Files,
    label: "File ownership",
    help: "Reads EphemeralOS per-line ownership from publish auditability through file_blame; this is not Git blame.",
    measured_boundary: "One request resolves ownership for a deterministic published line range; auditability fixture construction and range verification are outside request latency.",
    count_semantics_help: "Concurrent requests is the number of independent file_blame product requests released in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentRequests {
        factor: FactorId::ConcurrentRequests,
    },
    execution_shape: ExecutionShape::BarrierRequestBatch,
    isolation: IsolationPolicy::ReusableVerifiedFixture,
    cleanup: CleanupPolicy::VerifyFixtureUnchanged,
    product_access: ProductAccess::PublicGateway(ProductOperation::FileBlame),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::PublicReadOnly,
    factors: BLAME_FACTORS,
    checks: BLAME_CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: BLAME_COMPARISON_FACTORS,
    },
};

#[must_use]
pub fn validate_read(plan: &FileReadPlan) -> Vec<OperationValidationError> {
    let operation = OperationId::FileRead;
    let factors = &plan.factors;
    let mut errors = request_factor_errors(operation, &factors.concurrent_requests);
    errors.extend(validate_factor(
        operation,
        FactorId::ReturnedBytes,
        &factors.returned_bytes,
    ));
    errors.extend(validate_nonzero_u64(
        operation,
        FactorId::ReturnedBytes,
        &factors.returned_bytes,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::ReadSource,
        &factors.source,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::TargetMode,
        &factors.target_mode,
    ));
    errors
}

#[must_use]
pub fn validate_write(plan: &FileWritePlan) -> Vec<OperationValidationError> {
    let operation = OperationId::FileWrite;
    let factors = &plan.factors;
    let mut errors = request_factor_errors(operation, &factors.concurrent_requests);
    errors.extend(validate_factor(
        operation,
        FactorId::ContentBytes,
        &factors.content_bytes,
    ));
    errors.extend(validate_nonzero_u64(
        operation,
        FactorId::ContentBytes,
        &factors.content_bytes,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::MutationDestination,
        &factors.destination,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::TargetMode,
        &factors.target_mode,
    ));
    errors
}

#[must_use]
pub fn validate_edit(plan: &FileEditPlan) -> Vec<OperationValidationError> {
    let operation = OperationId::FileEdit;
    let factors = &plan.factors;
    let mut errors = request_factor_errors(operation, &factors.concurrent_requests);
    errors.extend(validate_factor(
        operation,
        FactorId::FileBytes,
        &factors.file_bytes,
    ));
    errors.extend(validate_nonzero_u64(
        operation,
        FactorId::FileBytes,
        &factors.file_bytes,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::ReplacementCount,
        &factors.replacement_count,
    ));
    errors.extend(validate_nonzero_u32(
        operation,
        FactorId::ReplacementCount,
        &factors.replacement_count,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::MatchDensity,
        &factors.match_density,
    ));
    errors.extend(validate_unit_ratio(
        operation,
        FactorId::MatchDensity,
        &factors.match_density,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::MutationDestination,
        &factors.destination,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::TargetMode,
        &factors.target_mode,
    ));
    errors
}

#[must_use]
pub fn validate_blame(plan: &FileBlamePlan) -> Vec<OperationValidationError> {
    let operation = OperationId::FileBlame;
    let factors = &plan.factors;
    let mut errors = request_factor_errors(operation, &factors.concurrent_requests);
    for (id, factor) in [
        (FactorId::LineCount, &factors.line_count),
        (FactorId::OwnershipSegments, &factors.ownership_segments),
        (
            FactorId::AuditabilityEventCount,
            &factors.auditability_event_count,
        ),
    ] {
        errors.extend(validate_factor(operation, id, factor));
        errors.extend(validate_nonzero_u32(operation, id, factor));
    }
    errors
}

pub fn expand_read(
    plan: &FileReadPlan,
) -> Result<Vec<FileReadCell>, Vec<OperationValidationError>> {
    let errors = validate_read(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }
    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &concurrent_requests in &factors.concurrent_requests.values {
        for &returned_bytes in &factors.returned_bytes.values {
            for &source in &factors.source.values {
                for &target_mode in &factors.target_mode.values {
                    cells.push(FileReadCell {
                        concurrent_requests,
                        returned_bytes,
                        source,
                        target_mode,
                        resolved_isolation: ResolvedIsolationPolicy::ReusableVerifiedFixture,
                    });
                }
            }
        }
    }
    Ok(cells)
}

pub fn expand_write(
    plan: &FileWritePlan,
) -> Result<Vec<FileWriteCell>, Vec<OperationValidationError>> {
    let errors = validate_write(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }
    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &concurrent_requests in &factors.concurrent_requests.values {
        for &content_bytes in &factors.content_bytes.values {
            for &destination in &factors.destination.values {
                for &target_mode in &factors.target_mode.values {
                    cells.push(FileWriteCell {
                        concurrent_requests,
                        content_bytes,
                        destination,
                        target_mode,
                        resolved_isolation: mutation_isolation(destination),
                    });
                }
            }
        }
    }
    Ok(cells)
}

pub fn expand_edit(
    plan: &FileEditPlan,
) -> Result<Vec<FileEditCell>, Vec<OperationValidationError>> {
    let errors = validate_edit(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }
    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &concurrent_requests in &factors.concurrent_requests.values {
        for &file_bytes in &factors.file_bytes.values {
            for &replacement_count in &factors.replacement_count.values {
                for &match_density in &factors.match_density.values {
                    for &destination in &factors.destination.values {
                        for &target_mode in &factors.target_mode.values {
                            cells.push(FileEditCell {
                                concurrent_requests,
                                file_bytes,
                                replacement_count,
                                match_density,
                                destination,
                                target_mode,
                                resolved_isolation: mutation_isolation(destination),
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(cells)
}

pub fn expand_blame(
    plan: &FileBlamePlan,
) -> Result<Vec<FileBlameCell>, Vec<OperationValidationError>> {
    let errors = validate_blame(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }
    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &concurrent_requests in &factors.concurrent_requests.values {
        for &line_count in &factors.line_count.values {
            for &ownership_segments in &factors.ownership_segments.values {
                for &auditability_event_count in &factors.auditability_event_count.values {
                    cells.push(FileBlameCell {
                        concurrent_requests,
                        line_count,
                        ownership_segments,
                        auditability_event_count,
                        resolved_isolation: ResolvedIsolationPolicy::ReusableVerifiedFixture,
                    });
                }
            }
        }
    }
    Ok(cells)
}

#[must_use]
pub const fn read_comparison_identity(cell: &FileReadCell) -> FileReadComparisonIdentity {
    FileReadComparisonIdentity {
        concurrent_requests: cell.concurrent_requests,
        returned_bytes: cell.returned_bytes,
        source: cell.source,
        target_mode: cell.target_mode,
    }
}

#[must_use]
pub const fn write_comparison_identity(cell: &FileWriteCell) -> FileWriteComparisonIdentity {
    FileWriteComparisonIdentity {
        concurrent_requests: cell.concurrent_requests,
        content_bytes: cell.content_bytes,
        destination: cell.destination,
        target_mode: cell.target_mode,
    }
}

#[must_use]
pub const fn edit_comparison_identity(cell: &FileEditCell) -> FileEditComparisonIdentity {
    FileEditComparisonIdentity {
        concurrent_requests: cell.concurrent_requests,
        file_bytes: cell.file_bytes,
        replacement_count: cell.replacement_count,
        match_density: cell.match_density,
        destination: cell.destination,
        target_mode: cell.target_mode,
    }
}

#[must_use]
pub const fn blame_comparison_identity(cell: &FileBlameCell) -> FileBlameComparisonIdentity {
    FileBlameComparisonIdentity {
        concurrent_requests: cell.concurrent_requests,
        line_count: cell.line_count,
        ownership_segments: cell.ownership_segments,
        auditability_event_count: cell.auditability_event_count,
    }
}

fn request_factor_errors(
    operation: OperationId,
    factor: &Factor<u32>,
) -> Vec<OperationValidationError> {
    let mut errors = validate_factor(operation, FactorId::ConcurrentRequests, factor);
    errors.extend(validate_nonzero_u32(
        operation,
        FactorId::ConcurrentRequests,
        factor,
    ));
    errors
}

const fn mutation_isolation(destination: MutationDestination) -> ResolvedIsolationPolicy {
    match destination {
        MutationDestination::Session => ResolvedIsolationPolicy::FreshSessionsPerTrial,
        MutationDestination::Publish => ResolvedIsolationPolicy::FreshSandboxPerTrial,
    }
}

pub(crate) const MAX_RUNTIME_CONTENT_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_READ_BYTES: u64 = 256 * 1024;
pub(crate) const MAX_RUNTIME_EDITS: u32 = 4_096;
const VERIFICATION_PAGE_LINES: u64 = 2_000;
const MAX_VERIFICATION_PAGES: u32 = 64;
const FIXTURE_LINE_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadWire {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WriteKind {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteWire {
    #[serde(rename = "type")]
    kind: WriteKind,
    path: String,
    bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditWire {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    edits_applied: u32,
    replacements: u32,
    bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlameWire {
    path: String,
    ranges: Vec<BlameRangeWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlameRangeWire {
    start_line: u32,
    line_count: u32,
    owner: String,
}

fn parse_read(
    value: Value,
    operation: OperationId,
) -> Result<(ReadWire, ResponseMetadata), ExecutorError> {
    let metadata = super::response_metadata(&value, ProductOutputStatus::Succeeded)?;
    let wire = serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
        operation,
        detail: "file_read output",
    })?;
    Ok((wire, metadata))
}

fn parse_write(
    value: Value,
    operation: OperationId,
) -> Result<(WriteWire, ResponseMetadata), ExecutorError> {
    let metadata = super::response_metadata(&value, ProductOutputStatus::Succeeded)?;
    let wire = serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
        operation,
        detail: "file_write output",
    })?;
    Ok((wire, metadata))
}

fn parse_edit(value: Value) -> Result<(EditWire, ResponseMetadata), ExecutorError> {
    let metadata = super::response_metadata(&value, ProductOutputStatus::Succeeded)?;
    let wire = serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
        operation: OperationId::FileEdit,
        detail: "file_edit output",
    })?;
    Ok((wire, metadata))
}

fn parse_blame(value: Value) -> Result<(BlameWire, ResponseMetadata), ExecutorError> {
    let metadata = super::response_metadata(&value, ProductOutputStatus::Succeeded)?;
    let wire = serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
        operation: OperationId::FileBlame,
        detail: "file_blame output",
    })?;
    Ok((wire, metadata))
}

fn fixture_path(
    context: &RuntimeContext,
    operation: &str,
    index: u32,
) -> Result<ProductPath, ExecutorError> {
    Ok(ProductPath::new(format!(
        ".eos-benchmark/{}/{operation}-{index}.txt",
        context.fixture_key()
    ))?)
}

fn deterministic_content(bytes: u64, seed: &str) -> Result<String, ExecutorError> {
    if bytes == 0 || bytes > MAX_RUNTIME_CONTENT_BYTES {
        return Err(ExecutorError::InvalidFixture {
            operation: OperationId::FileWrite,
            reason: "content size exceeds the bounded product contract",
        });
    }
    let bytes = usize::try_from(bytes).map_err(|_| ExecutorError::InvalidFixture {
        operation: OperationId::FileWrite,
        reason: "content size does not fit the host",
    })?;
    let digest = format!("{:x}", Sha256::digest(seed.as_bytes()));
    Ok(digest.bytes().cycle().take(bytes).map(char::from).collect())
}

fn deterministic_multiline_content(bytes: u64, seed: &str) -> Result<String, ExecutorError> {
    let mut content = deterministic_content(bytes, seed)?.into_bytes();
    let line_stride = FIXTURE_LINE_BYTES.saturating_sub(1);
    let mut boundary = line_stride.saturating_sub(1);
    while boundary.saturating_add(1) < content.len() {
        content[boundary] = b'\n';
        boundary = boundary.saturating_add(line_stride);
    }
    String::from_utf8(content)
        .map_err(|_| ExecutorError::InvalidRuntime("deterministic fixture was not valid UTF-8"))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn aggregate_hash<'a>(contents: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for content in contents {
        digest.update(content.len().to_le_bytes());
        digest.update(content.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

async fn setup_write(
    context: &RuntimeContext,
    operation: OperationId,
    path: &ProductPath,
    content: &str,
    request_id: &str,
) -> Result<Correlation, ExecutorError> {
    let correlation = context.correlation(request_id)?;
    let value = context
        .gateway()
        .file_write(context.sandbox_id(), None, path, content, &correlation)
        .await?;
    let (wire, _) = parse_write(value, operation)?;
    if !matches!(wire.kind, WriteKind::Create | WriteKind::Update)
        || wire.path != path.as_str()
        || wire.bytes_written != u64::try_from(content.len()).unwrap_or(u64::MAX)
    {
        return Err(ExecutorError::ResponseSchema {
            operation,
            detail: "fixture write values",
        });
    }
    Ok(correlation)
}

async fn read_exact(
    context: &RuntimeContext,
    operation: OperationId,
    session_id: Option<&WorkspaceSessionId>,
    path: &ProductPath,
    request_id: &str,
    expected_bytes: u64,
) -> Result<ReadWire, ExecutorError> {
    if expected_bytes == 0 || expected_bytes > MAX_RUNTIME_CONTENT_BYTES {
        return Err(ExecutorError::InvalidFixture {
            operation,
            reason: "verification read exceeds the bounded product contract",
        });
    }
    let capacity = usize::try_from(expected_bytes).map_err(|_| ExecutorError::InvalidFixture {
        operation,
        reason: "verification read size does not fit the host",
    })?;
    let mut content = String::with_capacity(capacity);
    let mut offset = 1_u64;
    let mut total_lines = None;
    for page in 0..MAX_VERIFICATION_PAGES {
        let value = context
            .gateway()
            .file_read(
                context.sandbox_id(),
                session_id,
                path,
                offset,
                VERIFICATION_PAGE_LINES,
                &context.correlation(format!("{request_id}-{page}"))?,
            )
            .await?;
        let (wire, _) = parse_read(value, operation)?;
        let page_shape_matches = wire.path == path.as_str()
            && wire.start_line == offset
            && (1..=VERIFICATION_PAGE_LINES).contains(&wire.num_lines)
            && wire.bytes_read == u64::try_from(wire.content.len()).unwrap_or(u64::MAX)
            && wire.total_bytes == expected_bytes
            && wire.total_lines >= wire.num_lines
            && total_lines.is_none_or(|known| known == wire.total_lines)
            && wire.truncated == wire.next_offset.is_some();
        if !page_shape_matches {
            return Err(ExecutorError::ResponseSchema {
                operation,
                detail: "paginated verification read values",
            });
        }
        total_lines = Some(wire.total_lines);
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&wire.content);
        let next_expected = offset.saturating_add(wire.num_lines);
        match wire.next_offset {
            Some(next) if next == next_expected && next <= wire.total_lines => {
                offset = next;
            }
            None if next_expected == wire.total_lines.saturating_add(1) => {
                let content_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
                if content_bytes != expected_bytes {
                    return Err(ExecutorError::ResponseSchema {
                        operation,
                        detail: "paginated verification read byte count",
                    });
                }
                return Ok(ReadWire {
                    path: wire.path,
                    content,
                    start_line: 1,
                    num_lines: wire.total_lines,
                    total_lines: wire.total_lines,
                    bytes_read: content_bytes,
                    total_bytes: wire.total_bytes,
                    next_offset: None,
                    truncated: false,
                });
            }
            _ => {
                return Err(ExecutorError::ResponseSchema {
                    operation,
                    detail: "paginated verification read continuation",
                });
            }
        }
    }
    Err(ExecutorError::ResponseSchema {
        operation,
        detail: "paginated verification read page limit",
    })
}

fn require_count(
    operation: OperationId,
    expected: u32,
    actual: usize,
) -> Result<(), ExecutorError> {
    if usize::try_from(expected).ok() != Some(actual) {
        return Err(ExecutorError::InvocationCount {
            operation,
            expected,
            actual,
        });
    }
    Ok(())
}

#[derive(Debug)]
pub struct FileReadRuntime;

#[derive(Debug)]
pub struct PreparedFileRead {
    paths: Vec<ProductPath>,
    contents: Vec<String>,
    session: Option<CreatedSession>,
    sessions: SessionRegistry,
    session_baseline: usize,
}

#[derive(Debug, Clone)]
pub struct FileReadInvocation {
    request_id: String,
    path: ProductPath,
    session_id: Option<WorkspaceSessionId>,
}

impl RuntimeInvocation for FileReadInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct FileReadOutput {
    pub metadata: ResponseMetadata,
    response: ReadWire,
}

impl RuntimeOutput for FileReadOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

impl OperationLifecycle for FileReadRuntime {
    type Cell = FileReadCell;
    type Prepared = PreparedFileRead;
    type Invocation = FileReadInvocation;
    type Output = FileReadOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.concurrent_requests == 0
            || cell.returned_bytes == 0
            || cell.returned_bytes > MAX_RUNTIME_READ_BYTES
        {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::FileRead,
                reason: "read factors exceed the bounded product contract",
            });
        }
        let target_count = match cell.target_mode {
            TargetMode::Independent => cell.concurrent_requests,
            TargetMode::SameTarget => 1,
        };
        let mut paths = Vec::new();
        let mut contents = Vec::new();
        for index in 0..target_count {
            let path = fixture_path(context, "read", index)?;
            let content = deterministic_content(
                cell.returned_bytes,
                &format!("read:{}:{index}", context.fixture_key()),
            )?;
            setup_write(
                context,
                OperationId::FileRead,
                &path,
                &content,
                &format!("prepare-read-{index}"),
            )
            .await?;
            paths.push(path);
            contents.push(content);
        }
        let session_baseline = context.workspace_sessions().owned_session_count()?;
        let sessions = session_registry();
        let session = match cell.source {
            FileReadSource::Snapshot => None,
            FileReadSource::Session => {
                let session = context
                    .workspace_sessions()
                    .create_no_op(
                        context.sandbox_id().clone(),
                        crate::model::AllowedNetworkProfile::Shared,
                        context.correlation("prepare-read-session")?,
                    )
                    .await?;
                register_session(&sessions, session.clone())?;
                Some(session)
            }
        };
        Ok(PreparedFileRead {
            paths,
            contents,
            session,
            sessions,
            session_baseline,
        })
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        let session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id().clone());
        Ok((0..cell.concurrent_requests)
            .map(|index| FileReadInvocation {
                request_id: format!("file-read-{index}"),
                path: prepared.paths[match cell.target_mode {
                    TargetMode::Independent => usize::try_from(index).unwrap_or(usize::MAX),
                    TargetMode::SameTarget => 0,
                }]
                .clone(),
                session_id: session_id.clone(),
            })
            .collect())
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let value = context
                .gateway()
                .file_read(
                    context.sandbox_id(),
                    invocation.session_id.as_ref(),
                    &invocation.path,
                    1,
                    1,
                    &context.correlation(&request_id)?,
                )
                .await?;
            let (response, metadata) = parse_read(value, OperationId::FileRead)?;
            Ok(FileReadOutput { metadata, response })
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
        require_count(
            OperationId::FileRead,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let mut checks = Vec::with_capacity(outcomes.len().saturating_mul(2));
        for (index, outcome) in outcomes.iter().enumerate() {
            let fixture_index = match cell.target_mode {
                TargetMode::Independent => index,
                TargetMode::SameTarget => 0,
            };
            let expected_path = prepared.paths[fixture_index].as_str();
            let expected_content = &prepared.contents[fixture_index];
            let started = Instant::now();
            let (window_passed, window_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => {
                    let wire = &output.response;
                    (
                        wire.path == expected_path
                            && wire.start_line == 1
                            && wire.num_lines == 1
                            && wire.total_lines == 1
                            && wire.next_offset.is_none()
                            && !wire.truncated,
                        format!(
                            "path={},start={},lines={}/{},next={:?},truncated={}",
                            wire.path,
                            wire.start_line,
                            wire.num_lines,
                            wire.total_lines,
                            wire.next_offset,
                            wire.truncated
                        ),
                    )
                }
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::FileRead,
                CheckId::FileReadWindow,
                Some(outcome.request_id().to_owned()),
                window_passed,
                format!("path={expected_path},start=1,lines=1/1,next=None,truncated=false"),
                window_actual,
                started,
            ));

            let started = Instant::now();
            let (hash_passed, hash_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => {
                    let wire = &output.response;
                    (
                        wire.content == *expected_content
                            && wire.bytes_read == cell.returned_bytes
                            && wire.total_bytes == cell.returned_bytes,
                        format!(
                            "bytes={}/{},hash={}",
                            wire.bytes_read,
                            wire.total_bytes,
                            sha256(wire.content.as_bytes())
                        ),
                    )
                }
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::FileRead,
                CheckId::FileContentHash,
                Some(outcome.request_id().to_owned()),
                hash_passed,
                format!(
                    "bytes={},hash={}",
                    cell.returned_bytes,
                    sha256(expected_content.as_bytes())
                ),
                hash_actual,
                started,
            ));
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
        _prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(
            OperationId::FileRead,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let output = outcomes.iter().find_map(InvocationOutcome::output).ok_or(
            ExecutorError::EvidenceUnavailable {
                operation: OperationId::FileRead,
                reason: "no successful read response",
            },
        )?;
        Ok(OperationEvidence::FileRead(FileReadEvidence {
            requested_bytes: cell.returned_bytes,
            returned_bytes: output.response.bytes_read,
            returned_lines: output.response.num_lines,
            content_sha256: sha256(output.response.content.as_bytes()),
        }))
    }
}

#[derive(Debug)]
pub struct FileWriteRuntime;

#[derive(Debug, Clone)]
struct MutationObserved {
    expected_sha256: String,
    observed_sha256: String,
    observed_bytes: u64,
    attributed_layer_count: u32,
}

#[derive(Debug)]
pub struct PreparedFileWrite {
    paths: Vec<ProductPath>,
    baseline_by_path: BTreeMap<String, String>,
    expected_by_path: BTreeMap<String, Vec<String>>,
    request_contents: Vec<String>,
    session: Option<CreatedSession>,
    sessions: SessionRegistry,
    session_baseline: usize,
    observed: Mutex<Option<MutationObserved>>,
}

#[derive(Debug, Clone)]
pub struct FileWriteInvocation {
    request_id: String,
    path: ProductPath,
    content: String,
    session_id: Option<WorkspaceSessionId>,
}

impl RuntimeInvocation for FileWriteInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct FileWriteOutput {
    pub metadata: ResponseMetadata,
    response: WriteWire,
}

impl RuntimeOutput for FileWriteOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

impl OperationLifecycle for FileWriteRuntime {
    type Cell = FileWriteCell;
    type Prepared = PreparedFileWrite;
    type Invocation = FileWriteInvocation;
    type Output = FileWriteOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.concurrent_requests == 0
            || cell.content_bytes == 0
            || cell.content_bytes > MAX_RUNTIME_CONTENT_BYTES
        {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::FileWrite,
                reason: "write factors exceed the bounded product contract",
            });
        }
        let target_count = match cell.target_mode {
            TargetMode::Independent => cell.concurrent_requests,
            TargetMode::SameTarget => 1,
        };
        let mut paths = Vec::new();
        let mut baseline_by_path = BTreeMap::new();
        for index in 0..target_count {
            let path = fixture_path(context, "write", index)?;
            let baseline = deterministic_multiline_content(
                cell.content_bytes,
                &format!("write-baseline:{}:{index}", context.fixture_key()),
            )?;
            setup_write(
                context,
                OperationId::FileWrite,
                &path,
                &baseline,
                &format!("prepare-write-{index}"),
            )
            .await?;
            baseline_by_path.insert(path.as_str().to_owned(), baseline);
            paths.push(path);
        }
        let mut request_contents = Vec::new();
        let mut expected_by_path = BTreeMap::<String, Vec<String>>::new();
        for index in 0..cell.concurrent_requests {
            let content = deterministic_multiline_content(
                cell.content_bytes,
                &format!("write-request:{}:{index}", context.fixture_key()),
            )?;
            let path = &paths[match cell.target_mode {
                TargetMode::Independent => usize::try_from(index).unwrap_or(usize::MAX),
                TargetMode::SameTarget => 0,
            }];
            expected_by_path
                .entry(path.as_str().to_owned())
                .or_default()
                .push(content.clone());
            request_contents.push(content);
        }
        let sessions = session_registry();
        let session_baseline = context.workspace_sessions().owned_session_count()?;
        let session = match cell.destination {
            MutationDestination::Publish => None,
            MutationDestination::Session => {
                let session = context
                    .workspace_sessions()
                    .create_no_op(
                        context.sandbox_id().clone(),
                        crate::model::AllowedNetworkProfile::Shared,
                        context.correlation("prepare-write-session")?,
                    )
                    .await?;
                register_session(&sessions, session.clone())?;
                Some(session)
            }
        };
        Ok(PreparedFileWrite {
            paths,
            baseline_by_path,
            expected_by_path,
            request_contents,
            session,
            sessions,
            session_baseline,
            observed: Mutex::new(None),
        })
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        let session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id().clone());
        Ok((0..cell.concurrent_requests)
            .map(|index| FileWriteInvocation {
                request_id: format!("file-write-{index}"),
                path: prepared.paths[match cell.target_mode {
                    TargetMode::Independent => usize::try_from(index).unwrap_or(usize::MAX),
                    TargetMode::SameTarget => 0,
                }]
                .clone(),
                content: prepared.request_contents[usize::try_from(index).unwrap_or(usize::MAX)]
                    .clone(),
                session_id: session_id.clone(),
            })
            .collect())
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let value = context
                .gateway()
                .file_write(
                    context.sandbox_id(),
                    invocation.session_id.as_ref(),
                    &invocation.path,
                    &invocation.content,
                    &context.correlation(&request_id)?,
                )
                .await?;
            let (response, metadata) = parse_write(value, OperationId::FileWrite)?;
            Ok(FileWriteOutput { metadata, response })
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
        require_count(
            OperationId::FileWrite,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let response_values_match = outcomes.iter().enumerate().all(|(index, outcome)| {
            outcome.output().is_some_and(|output| {
                let expected_path = prepared.paths[match cell.target_mode {
                    TargetMode::Independent => index,
                    TargetMode::SameTarget => 0,
                }]
                .as_str();
                output.response.kind == WriteKind::Update
                    && output.response.path == expected_path
                    && output.response.bytes_written == cell.content_bytes
            })
        });
        let session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id());
        let mut actual_by_path = BTreeMap::new();
        let mut observed_bytes = None;
        let mut content_matches = response_values_match;
        for (index, path) in prepared.paths.iter().enumerate() {
            let wire = read_exact(
                context,
                OperationId::FileWrite,
                session_id,
                path,
                &format!("verify-write-content-{index}"),
                cell.content_bytes,
            )
            .await?;
            let allowed = prepared
                .expected_by_path
                .get(path.as_str())
                .is_some_and(|contents| contents.contains(&wire.content));
            content_matches &= wire.path == path.as_str()
                && wire.bytes_read == cell.content_bytes
                && wire.total_bytes == cell.content_bytes
                && allowed;
            observed_bytes.get_or_insert(wire.bytes_read);
            actual_by_path.insert(path.as_str().to_owned(), wire.content);
        }
        let observed_bytes = observed_bytes.ok_or(ExecutorError::EvidenceUnavailable {
            operation: OperationId::FileWrite,
            reason: "no post-write content was observed",
        })?;

        let mut attribution_matches = true;
        let attributed_layer_count = match cell.destination {
            MutationDestination::Session => {
                for (index, path) in prepared.paths.iter().enumerate() {
                    let snapshot = read_exact(
                        context,
                        OperationId::FileWrite,
                        None,
                        path,
                        &format!("verify-write-snapshot-{index}"),
                        cell.content_bytes,
                    )
                    .await?;
                    attribution_matches &= prepared
                        .baseline_by_path
                        .get(path.as_str())
                        .is_some_and(|baseline| {
                            snapshot.path == path.as_str()
                                && snapshot.bytes_read == cell.content_bytes
                                && snapshot.total_bytes == cell.content_bytes
                                && snapshot.content == *baseline
                        });
                }
                0
            }
            MutationDestination::Publish => {
                for (index, path) in prepared.paths.iter().enumerate() {
                    let allowed_owners = match cell.target_mode {
                        TargetMode::Independent => BTreeSet::from([format!(
                            "operation:{}",
                            context
                                .correlation(format!("file-write-{index}"))?
                                .wire_request_id()
                        )]),
                        TargetMode::SameTarget => (0..cell.concurrent_requests)
                            .map(|request_index| {
                                context
                                    .correlation(format!("file-write-{request_index}"))
                                    .map(|correlation| {
                                        format!("operation:{}", correlation.wire_request_id())
                                    })
                            })
                            .collect::<Result<BTreeSet<_>, _>>()?,
                    };
                    let value = context
                        .gateway()
                        .file_blame(
                            context.sandbox_id(),
                            path,
                            &context.correlation(format!("verify-write-blame-{index}"))?,
                        )
                        .await?;
                    let (blame, _) = parse_blame(value)?;
                    let expected_line_count = actual_by_path
                        .get(path.as_str())
                        .map(|content| {
                            u32::try_from(content.split('\n').count()).unwrap_or(u32::MAX)
                        })
                        .unwrap_or(0);
                    let owner_matches = blame.ranges.len() == 1
                        && blame.ranges[0].start_line == 1
                        && blame.ranges[0].line_count == expected_line_count
                        && allowed_owners.contains(&blame.ranges[0].owner);
                    attribution_matches &= blame.path == path.as_str() && owner_matches;
                }
                u32::try_from(
                    outcomes
                        .iter()
                        .filter(|outcome| outcome.is_success())
                        .count(),
                )
                .unwrap_or(u32::MAX)
            }
        };

        let expected_contents = prepared.paths.iter().filter_map(|path| {
            let actual = actual_by_path.get(path.as_str())?;
            prepared
                .expected_by_path
                .get(path.as_str())?
                .iter()
                .find(|expected| *expected == actual)
                .map(String::as_str)
        });
        let expected_sha256 = aggregate_hash(expected_contents);
        let observed_sha256 = aggregate_hash(
            prepared
                .paths
                .iter()
                .filter_map(|path| actual_by_path.get(path.as_str()).map(String::as_str)),
        );
        *prepared
            .observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)? = Some(MutationObserved {
            expected_sha256: expected_sha256.clone(),
            observed_sha256: observed_sha256.clone(),
            observed_bytes,
            attributed_layer_count,
        });

        let mut checks = Vec::with_capacity(2);
        let started = Instant::now();
        checks.push(check_result(
            context,
            OperationId::FileWrite,
            CheckId::FileContentHash,
            None,
            content_matches && expected_sha256 == observed_sha256,
            format!("hash={expected_sha256},bytes={}", cell.content_bytes),
            format!("hash={observed_sha256},bytes={observed_bytes}"),
            started,
        ));
        let started = Instant::now();
        checks.push(check_result(
            context,
            OperationId::FileWrite,
            CheckId::MutationAttribution,
            None,
            attribution_matches,
            format!("destination={:?}", cell.destination),
            format!(
                "destination={:?},attributed_layers={attributed_layer_count}",
                cell.destination
            ),
            started,
        ));
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
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(
            OperationId::FileWrite,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let observed = prepared
            .observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
            .clone()
            .ok_or(ExecutorError::EvidenceUnavailable {
                operation: OperationId::FileWrite,
                reason: "post-write verification did not complete",
            })?;
        Ok(OperationEvidence::FileWrite(FileWriteEvidence {
            requested_bytes: cell.content_bytes,
            observed_bytes: observed.observed_bytes,
            expected_sha256: observed.expected_sha256,
            observed_sha256: observed.observed_sha256,
            attribution: match cell.destination {
                MutationDestination::Session => MutationAttribution::WorkspaceSession,
                MutationDestination::Publish => MutationAttribution::PublishedOperationLayer,
            },
            attributed_layer_count: observed.attributed_layer_count,
        }))
    }
}

#[derive(Debug)]
pub struct FileEditRuntime;

#[derive(Debug, Clone)]
struct EditToken {
    old: String,
    new: String,
}

#[derive(Debug, Clone)]
struct EditRequestFixture {
    edits: Vec<ProductEdit>,
    expected_replacements: u32,
}

#[derive(Debug, Clone)]
struct EditObserved {
    before_sha256: String,
    expected_sha256: String,
    observed_sha256: String,
    applied_replacements: u32,
    attributed_layer_count: u32,
}

#[derive(Debug)]
pub struct PreparedFileEdit {
    paths: Vec<ProductPath>,
    before_by_path: BTreeMap<String, String>,
    expected_by_path: BTreeMap<String, String>,
    requests: Vec<EditRequestFixture>,
    session: Option<CreatedSession>,
    sessions: SessionRegistry,
    session_baseline: usize,
    observed: Mutex<Option<EditObserved>>,
}

#[derive(Debug, Clone)]
pub struct FileEditInvocation {
    request_id: String,
    path: ProductPath,
    edits: Vec<ProductEdit>,
    expected_replacements: u32,
    session_id: Option<WorkspaceSessionId>,
}

impl RuntimeInvocation for FileEditInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct FileEditOutput {
    pub metadata: ResponseMetadata,
    response: EditWire,
}

impl RuntimeOutput for FileEditOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

fn edit_token(seed: &str, request_index: u32, edit_index: u32) -> EditToken {
    let old = format!(
        "{:x}",
        Sha256::digest(format!("old:{seed}:{request_index}:{edit_index}"))
    );
    let new = format!(
        "{:x}",
        Sha256::digest(format!("new:{seed}:{request_index}:{edit_index}"))
    );
    EditToken { old, new }
}

fn build_edit_content(
    operation: OperationId,
    file_bytes: u64,
    match_density: UnitRatio,
    tokens: &[EditToken],
) -> Result<(String, String, BTreeMap<String, u32>), ExecutorError> {
    if file_bytes == 0
        || file_bytes > MAX_RUNTIME_CONTENT_BYTES
        || tokens.is_empty()
        || !match_density.is_valid()
    {
        return Err(ExecutorError::InvalidFixture {
            operation,
            reason: "edit fixture factors exceed the bounded product contract",
        });
    }
    let bytes = usize::try_from(file_bytes).map_err(|_| ExecutorError::InvalidFixture {
        operation,
        reason: "edit fixture size does not fit the host",
    })?;
    let token_bytes = tokens[0].old.len();
    let record_bytes = token_bytes.saturating_add(1);
    let slots = bytes.saturating_add(1) / record_bytes;
    let density_slots = ((slots as f64) * match_density.0).floor() as usize;
    if density_slots < tokens.len() || slots < tokens.len() {
        return Err(ExecutorError::InvalidFixture {
            operation,
            reason: "match density cannot place every requested exact replacement",
        });
    }

    let mut before = String::with_capacity(bytes);
    let mut counts = BTreeMap::<String, u32>::new();
    for slot in 0..slots {
        if slot < density_slots {
            let token = &tokens[slot % tokens.len()];
            before.push_str(&token.old);
            let count = counts.entry(token.old.clone()).or_default();
            *count = count.saturating_add(1);
        } else {
            before.extend(std::iter::repeat_n('~', token_bytes));
        }
        if slot.saturating_add(1) < slots {
            before.push('\n');
        }
    }
    before.extend(std::iter::repeat_n('~', bytes.saturating_sub(before.len())));
    let mut expected = before.clone();
    for token in tokens {
        expected = expected.replace(&token.old, &token.new);
    }
    Ok((before, expected, counts))
}

impl OperationLifecycle for FileEditRuntime {
    type Cell = FileEditCell;
    type Prepared = PreparedFileEdit;
    type Invocation = FileEditInvocation;
    type Output = FileEditOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.concurrent_requests == 0
            || cell.replacement_count == 0
            || cell.replacement_count > MAX_RUNTIME_EDITS
            || cell.file_bytes == 0
            || cell.file_bytes > MAX_RUNTIME_CONTENT_BYTES
            || !cell.match_density.is_valid()
        {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::FileEdit,
                reason: "edit factors exceed the bounded product contract",
            });
        }
        let target_count = match cell.target_mode {
            TargetMode::Independent => cell.concurrent_requests,
            TargetMode::SameTarget => 1,
        };
        let all_tokens: Vec<Vec<EditToken>> = (0..cell.concurrent_requests)
            .map(|request_index| {
                (0..cell.replacement_count)
                    .map(|edit_index| edit_token(&context.fixture_key(), request_index, edit_index))
                    .collect()
            })
            .collect();

        let mut paths = Vec::new();
        let mut before_by_path = BTreeMap::new();
        let mut expected_by_path = BTreeMap::new();
        let mut replacements_by_request = vec![0_u32; all_tokens.len()];
        for path_index in 0..target_count {
            let path = fixture_path(context, "edit", path_index)?;
            let request_indices: Vec<usize> = match cell.target_mode {
                TargetMode::Independent => vec![usize::try_from(path_index).unwrap_or(usize::MAX)],
                TargetMode::SameTarget => (0..all_tokens.len()).collect(),
            };
            let path_tokens: Vec<EditToken> = request_indices
                .iter()
                .flat_map(|index| all_tokens[*index].iter().cloned())
                .collect();
            let (before, expected, counts) = build_edit_content(
                OperationId::FileEdit,
                cell.file_bytes,
                cell.match_density,
                &path_tokens,
            )?;
            for request_index in request_indices {
                replacements_by_request[request_index] = all_tokens[request_index]
                    .iter()
                    .map(|token| counts.get(&token.old).copied().unwrap_or(0))
                    .fold(0_u32, u32::saturating_add);
            }
            setup_write(
                context,
                OperationId::FileEdit,
                &path,
                &before,
                &format!("prepare-edit-{path_index}"),
            )
            .await?;
            before_by_path.insert(path.as_str().to_owned(), before);
            expected_by_path.insert(path.as_str().to_owned(), expected);
            paths.push(path);
        }

        let requests = all_tokens
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| {
                let edits = tokens
                    .into_iter()
                    .map(|token| ProductEdit::new(token.old, token.new, true))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(EditRequestFixture {
                    edits,
                    expected_replacements: replacements_by_request[index],
                })
            })
            .collect::<Result<Vec<_>, crate::gateway::GatewayError>>()?;

        let sessions = session_registry();
        let session_baseline = context.workspace_sessions().owned_session_count()?;
        let session = match cell.destination {
            MutationDestination::Publish => None,
            MutationDestination::Session => {
                let session = context
                    .workspace_sessions()
                    .create_no_op(
                        context.sandbox_id().clone(),
                        crate::model::AllowedNetworkProfile::Shared,
                        context.correlation("prepare-edit-session")?,
                    )
                    .await?;
                register_session(&sessions, session.clone())?;
                Some(session)
            }
        };
        Ok(PreparedFileEdit {
            paths,
            before_by_path,
            expected_by_path,
            requests,
            session,
            sessions,
            session_baseline,
            observed: Mutex::new(None),
        })
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        let session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id().clone());
        Ok((0..cell.concurrent_requests)
            .map(|index| {
                let request_index = usize::try_from(index).unwrap_or(usize::MAX);
                FileEditInvocation {
                    request_id: format!("file-edit-{index}"),
                    path: prepared.paths[match cell.target_mode {
                        TargetMode::Independent => request_index,
                        TargetMode::SameTarget => 0,
                    }]
                    .clone(),
                    edits: prepared.requests[request_index].edits.clone(),
                    expected_replacements: prepared.requests[request_index].expected_replacements,
                    session_id: session_id.clone(),
                }
            })
            .collect())
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let value = context
                .gateway()
                .file_edit(
                    context.sandbox_id(),
                    invocation.session_id.as_ref(),
                    &invocation.path,
                    &invocation.edits,
                    &context.correlation(&request_id)?,
                )
                .await?;
            let (response, metadata) = parse_edit(value)?;
            if response.replacements != invocation.expected_replacements {
                return Err(ExecutorError::ResponseSchema {
                    operation: OperationId::FileEdit,
                    detail: "file_edit replacement count",
                });
            }
            Ok(FileEditOutput { metadata, response })
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
        require_count(
            OperationId::FileEdit,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let mut checks = Vec::with_capacity(outcomes.len().saturating_add(2));
        let mut response_values_match = true;
        for (index, outcome) in outcomes.iter().enumerate() {
            let expected_path = prepared.paths[match cell.target_mode {
                TargetMode::Independent => index,
                TargetMode::SameTarget => 0,
            }]
            .as_str();
            let started = Instant::now();
            let (passed, actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => (
                    output.response.kind == "edit"
                        && output.response.path == expected_path
                        && output.response.edits_applied == cell.replacement_count
                        && output.response.replacements
                            == prepared.requests[index].expected_replacements
                        && output.response.bytes_written == cell.file_bytes,
                    format!(
                        "path={},edits={},replacements={},bytes={}",
                        output.response.path,
                        output.response.edits_applied,
                        output.response.replacements,
                        output.response.bytes_written
                    ),
                ),
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            response_values_match &= passed;
            checks.push(check_result(
                context,
                OperationId::FileEdit,
                CheckId::FileEditReplacementCount,
                Some(outcome.request_id().to_owned()),
                passed,
                format!(
                    "path={expected_path},edits={},replacements={},bytes={}",
                    cell.replacement_count,
                    prepared.requests[index].expected_replacements,
                    cell.file_bytes
                ),
                actual,
                started,
            ));
        }

        let session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id());
        let mut observed_by_path = BTreeMap::new();
        let mut content_matches = response_values_match;
        for (index, path) in prepared.paths.iter().enumerate() {
            let read = read_exact(
                context,
                OperationId::FileEdit,
                session_id,
                path,
                &format!("verify-edit-content-{index}"),
                cell.file_bytes,
            )
            .await?;
            let expected = &prepared.expected_by_path[path.as_str()];
            content_matches &= read.path == path.as_str()
                && read.content == *expected
                && read.bytes_read == cell.file_bytes
                && read.total_bytes == cell.file_bytes;
            observed_by_path.insert(path.as_str().to_owned(), read.content);
        }

        let mut attribution_matches = true;
        let attributed_layer_count = match cell.destination {
            MutationDestination::Session => {
                for (index, path) in prepared.paths.iter().enumerate() {
                    let snapshot = read_exact(
                        context,
                        OperationId::FileEdit,
                        None,
                        path,
                        &format!("verify-edit-snapshot-{index}"),
                        cell.file_bytes,
                    )
                    .await?;
                    attribution_matches &=
                        prepared
                            .before_by_path
                            .get(path.as_str())
                            .is_some_and(|before| {
                                snapshot.path == path.as_str()
                                    && snapshot.bytes_read == cell.file_bytes
                                    && snapshot.total_bytes == cell.file_bytes
                                    && snapshot.content == *before
                            });
                }
                0
            }
            MutationDestination::Publish => {
                for (index, path) in prepared.paths.iter().enumerate() {
                    let mutation_owners = match cell.target_mode {
                        TargetMode::Independent => BTreeSet::from([format!(
                            "operation:{}",
                            context
                                .correlation(format!("file-edit-{index}"))?
                                .wire_request_id()
                        )]),
                        TargetMode::SameTarget => (0..cell.concurrent_requests)
                            .map(|request_index| {
                                context
                                    .correlation(format!("file-edit-{request_index}"))
                                    .map(|correlation| {
                                        format!("operation:{}", correlation.wire_request_id())
                                    })
                            })
                            .collect::<Result<BTreeSet<_>, _>>()?,
                    };
                    let baseline_index = match cell.target_mode {
                        TargetMode::Independent => index,
                        TargetMode::SameTarget => 0,
                    };
                    let baseline_owner = format!(
                        "operation:{}",
                        context
                            .correlation(format!("prepare-edit-{baseline_index}"))?
                            .wire_request_id()
                    );
                    let value = context
                        .gateway()
                        .file_blame(
                            context.sandbox_id(),
                            path,
                            &context.correlation(format!("verify-edit-blame-{index}"))?,
                        )
                        .await?;
                    let (blame, _) = parse_blame(value)?;
                    let saw_mutation = blame
                        .ranges
                        .iter()
                        .any(|range| mutation_owners.contains(&range.owner));
                    let all_owners_known = blame.ranges.iter().all(|range| {
                        mutation_owners.contains(&range.owner) || range.owner == baseline_owner
                    });
                    attribution_matches &= blame.path == path.as_str()
                        && !blame.ranges.is_empty()
                        && saw_mutation
                        && all_owners_known;
                }
                u32::try_from(
                    outcomes
                        .iter()
                        .filter(|outcome| outcome.is_success())
                        .count(),
                )
                .unwrap_or(u32::MAX)
            }
        };

        let before_sha256 = aggregate_hash(prepared.paths.iter().filter_map(|path| {
            prepared
                .before_by_path
                .get(path.as_str())
                .map(String::as_str)
        }));
        let expected_sha256 = aggregate_hash(prepared.paths.iter().filter_map(|path| {
            prepared
                .expected_by_path
                .get(path.as_str())
                .map(String::as_str)
        }));
        let observed_sha256 = aggregate_hash(
            prepared
                .paths
                .iter()
                .filter_map(|path| observed_by_path.get(path.as_str()).map(String::as_str)),
        );
        let applied_replacements = outcomes
            .iter()
            .find_map(InvocationOutcome::output)
            .map(|output| output.response.edits_applied);
        if let Some(applied_replacements) = applied_replacements {
            *prepared
                .observed
                .lock()
                .map_err(|_| ExecutorError::SessionRegistryUnavailable)? = Some(EditObserved {
                before_sha256: before_sha256.clone(),
                expected_sha256: expected_sha256.clone(),
                observed_sha256: observed_sha256.clone(),
                applied_replacements,
                attributed_layer_count,
            });
        }

        let started = Instant::now();
        checks.push(check_result(
            context,
            OperationId::FileEdit,
            CheckId::FileContentHash,
            None,
            content_matches && expected_sha256 == observed_sha256,
            format!("hash={expected_sha256},bytes={}", cell.file_bytes),
            format!("hash={observed_sha256},bytes={}", cell.file_bytes),
            started,
        ));
        let started = Instant::now();
        checks.push(check_result(
            context,
            OperationId::FileEdit,
            CheckId::MutationAttribution,
            None,
            attribution_matches,
            format!("destination={:?}", cell.destination),
            format!(
                "destination={:?},attributed_layers={attributed_layer_count}",
                cell.destination
            ),
            started,
        ));
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
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(
            OperationId::FileEdit,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let observed = prepared
            .observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
            .clone()
            .ok_or(ExecutorError::EvidenceUnavailable {
                operation: OperationId::FileEdit,
                reason: "post-edit verification did not complete",
            })?;
        Ok(OperationEvidence::FileEdit(FileEditEvidence {
            requested_replacements: cell.replacement_count,
            applied_replacements: observed.applied_replacements,
            before_sha256: observed.before_sha256,
            expected_sha256: observed.expected_sha256,
            observed_sha256: observed.observed_sha256,
            attribution: match cell.destination {
                MutationDestination::Session => MutationAttribution::WorkspaceSession,
                MutationDestination::Publish => MutationAttribution::PublishedOperationLayer,
            },
            attributed_layer_count: observed.attributed_layer_count,
        }))
    }
}

#[derive(Debug)]
pub struct FileBlameRuntime;

#[derive(Debug)]
pub struct PreparedFileBlame {
    path: ProductPath,
    expected_ranges: Vec<BlameRangeWire>,
}

#[derive(Debug, Clone)]
pub struct FileBlameInvocation {
    request_id: String,
    path: ProductPath,
}

impl RuntimeInvocation for FileBlameInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct FileBlameOutput {
    pub metadata: ResponseMetadata,
    response: BlameWire,
}

impl RuntimeOutput for FileBlameOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

pub(crate) const MAX_AUDITABILITY_EVENTS: u32 = 4_096;

fn blame_content(line_count: u32, segments: u32, event: u32) -> String {
    let base = line_count / segments;
    let remainder = line_count % segments;
    let mut lines = Vec::with_capacity(usize::try_from(line_count).unwrap_or(0));
    let mut line = 0_u32;
    for segment in 0..segments {
        let segment_lines = base + u32::from(segment < remainder);
        let prefix = if segment % 2 == 0 { 'A' } else { 'B' };
        for _ in 0..segment_lines {
            lines.push(format!("{prefix}|{event:08x}|{line:08x}"));
            line = line.saturating_add(1);
        }
    }
    lines.join("\n")
}

fn expected_blame_ranges(
    context: &RuntimeContext,
    cell: &FileBlameCell,
) -> Result<Vec<BlameRangeWire>, ExecutorError> {
    let final_event = cell.auditability_event_count.saturating_sub(1);
    if cell.ownership_segments == 1 {
        return Ok(vec![BlameRangeWire {
            start_line: 1,
            line_count: cell.line_count,
            owner: format!(
                "operation:{}",
                context
                    .correlation(format!("prepare-blame-event-{final_event}"))?
                    .wire_request_id()
            ),
        }]);
    }
    let previous_event = final_event.saturating_sub(1);
    let final_owner = format!(
        "operation:{}",
        context
            .correlation(format!("prepare-blame-event-{final_event}"))?
            .wire_request_id()
    );
    let previous_owner = format!(
        "operation:{}",
        context
            .correlation(format!("prepare-blame-event-{previous_event}"))?
            .wire_request_id()
    );
    let base = cell.line_count / cell.ownership_segments;
    let remainder = cell.line_count % cell.ownership_segments;
    let mut start_line = 1_u32;
    Ok((0..cell.ownership_segments)
        .map(|segment| {
            let line_count = base + u32::from(segment < remainder);
            let range = BlameRangeWire {
                start_line,
                line_count,
                owner: if segment % 2 == 0 {
                    final_owner.clone()
                } else {
                    previous_owner.clone()
                },
            };
            start_line = start_line.saturating_add(line_count);
            range
        })
        .collect())
}

impl OperationLifecycle for FileBlameRuntime {
    type Cell = FileBlameCell;
    type Prepared = PreparedFileBlame;
    type Invocation = FileBlameInvocation;
    type Output = FileBlameOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.concurrent_requests == 0
            || cell.line_count == 0
            || cell.ownership_segments == 0
            || cell.ownership_segments > cell.line_count
            || cell.auditability_event_count == 0
            || cell.auditability_event_count > MAX_AUDITABILITY_EVENTS
            || (cell.ownership_segments > 1 && cell.auditability_event_count < 2)
        {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::FileBlame,
                reason: "blame factors cannot form the requested ownership audit fixture",
            });
        }
        let path = fixture_path(context, "blame", 0)?;
        if cell.ownership_segments == 1 {
            for event in 0..cell.auditability_event_count {
                let content = blame_content(cell.line_count, 1, event);
                if content.len() > usize::try_from(MAX_RUNTIME_CONTENT_BYTES).unwrap_or(usize::MAX)
                {
                    return Err(ExecutorError::InvalidFixture {
                        operation: OperationId::FileBlame,
                        reason: "blame fixture exceeds the bounded product contract",
                    });
                }
                setup_write(
                    context,
                    OperationId::FileBlame,
                    &path,
                    &content,
                    &format!("prepare-blame-event-{event}"),
                )
                .await?;
            }
        } else {
            let write_events = cell.auditability_event_count.saturating_sub(1);
            for event in 0..write_events {
                let content = blame_content(cell.line_count, cell.ownership_segments, event);
                if content.len() > usize::try_from(MAX_RUNTIME_CONTENT_BYTES).unwrap_or(usize::MAX)
                {
                    return Err(ExecutorError::InvalidFixture {
                        operation: OperationId::FileBlame,
                        reason: "blame fixture exceeds the bounded product contract",
                    });
                }
                setup_write(
                    context,
                    OperationId::FileBlame,
                    &path,
                    &content,
                    &format!("prepare-blame-event-{event}"),
                )
                .await?;
            }
            let final_event = cell.auditability_event_count.saturating_sub(1);
            let edit = ProductEdit::new("A|", "C|", true)?;
            let value = context
                .gateway()
                .file_edit(
                    context.sandbox_id(),
                    None,
                    &path,
                    &[edit],
                    &context.correlation(format!("prepare-blame-event-{final_event}"))?,
                )
                .await?;
            let (wire, _) = parse_edit(value)?;
            let changed_lines = (0..cell.ownership_segments)
                .filter(|segment| segment % 2 == 0)
                .map(|segment| {
                    cell.line_count / cell.ownership_segments
                        + u32::from(segment < cell.line_count % cell.ownership_segments)
                })
                .fold(0_u32, u32::saturating_add);
            let expected_bytes = u64::try_from(
                blame_content(cell.line_count, cell.ownership_segments, final_event).len(),
            )
            .unwrap_or(u64::MAX);
            if wire.kind != "edit"
                || wire.path != path.as_str()
                || wire.edits_applied != 1
                || wire.replacements != changed_lines
                || wire.bytes_written != expected_bytes
            {
                return Err(ExecutorError::ResponseSchema {
                    operation: OperationId::FileBlame,
                    detail: "blame audit fixture edit values",
                });
            }
        }
        Ok(PreparedFileBlame {
            path,
            expected_ranges: expected_blame_ranges(context, cell)?,
        })
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        Ok((0..cell.concurrent_requests)
            .map(|index| FileBlameInvocation {
                request_id: format!("file-blame-{index}"),
                path: prepared.path.clone(),
            })
            .collect())
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let value = context
                .gateway()
                .file_blame(
                    context.sandbox_id(),
                    &invocation.path,
                    &context.correlation(&request_id)?,
                )
                .await?;
            let (response, metadata) = parse_blame(value)?;
            Ok(FileBlameOutput { metadata, response })
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
        require_count(
            OperationId::FileBlame,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let mut checks = Vec::with_capacity(outcomes.len().saturating_mul(2));
        for outcome in outcomes {
            let started = Instant::now();
            let (coverage_passed, coverage_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => {
                    let mut next_line = 1_u32;
                    let covered = output.response.ranges.iter().fold(0_u32, |total, range| {
                        let valid_start = range.start_line == next_line && range.line_count > 0;
                        if valid_start {
                            next_line = next_line.saturating_add(range.line_count);
                        } else {
                            next_line = u32::MAX;
                        }
                        total.saturating_add(range.line_count)
                    });
                    (
                        output.response.path == prepared.path.as_str()
                            && next_line == cell.line_count.saturating_add(1)
                            && covered == cell.line_count,
                        format!(
                            "path={},ranges={},covered={covered},next={next_line}",
                            output.response.path,
                            output.response.ranges.len()
                        ),
                    )
                }
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::FileBlame,
                CheckId::BlameRangeCoverage,
                Some(outcome.request_id().to_owned()),
                coverage_passed,
                format!(
                    "path={},covered={}",
                    prepared.path.as_str(),
                    cell.line_count
                ),
                coverage_actual,
                started,
            ));

            let started = Instant::now();
            let (ownership_passed, ownership_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => (
                    output.response.ranges == prepared.expected_ranges,
                    format!("ranges={:?}", output.response.ranges),
                ),
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::FileBlame,
                CheckId::BlameOwnership,
                Some(outcome.request_id().to_owned()),
                ownership_passed,
                format!("ranges={:?}", prepared.expected_ranges),
                ownership_actual,
                started,
            ));
        }
        Ok(Verification { checks })
    }

    async fn teardown(_context: &RuntimeContext, _prepared: &mut Self::Prepared) -> TeardownResult {
        TeardownResult::empty()
    }

    fn evidence(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(
            OperationId::FileBlame,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let output = outcomes.iter().find_map(InvocationOutcome::output).ok_or(
            ExecutorError::EvidenceUnavailable {
                operation: OperationId::FileBlame,
                reason: "no successful blame response",
            },
        )?;
        let covered_lines = output
            .response
            .ranges
            .iter()
            .map(|range| range.line_count)
            .fold(0_u32, u32::saturating_add);
        let matched = output
            .response
            .ranges
            .iter()
            .zip(&prepared.expected_ranges)
            .filter(|(actual, expected)| actual == expected)
            .count();
        Ok(OperationEvidence::FileBlame(FileBlameEvidence {
            requested_lines: cell.line_count,
            returned_ranges: u32::try_from(output.response.ranges.len()).unwrap_or(u32::MAX),
            covered_lines,
            expected_ownership_segments: cell.ownership_segments,
            matched_ownership_segments: u32::try_from(matched).unwrap_or(u32::MAX),
            observed_auditability_events: cell.auditability_event_count,
        }))
    }
}

#[cfg(test)]
mod runtime_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn edit_fixture_is_exact_deterministic_and_fully_replaced() {
        let tokens = vec![edit_token("fixture", 0, 0), edit_token("fixture", 0, 1)];
        let first = build_edit_content(OperationId::FileEdit, 649, UnitRatio(0.5), &tokens)
            .expect("valid edit fixture");
        let second = build_edit_content(OperationId::FileEdit, 649, UnitRatio(0.5), &tokens)
            .expect("same edit fixture");

        assert_eq!(first, second);
        assert_eq!(first.0.len(), 649);
        assert_eq!(first.1.len(), 649);
        assert_ne!(first.0, first.1);
        assert_eq!(first.2.values().copied().sum::<u32>(), 5);
        for token in &tokens {
            assert!(!first.1.contains(&token.old));
        }
    }

    #[test]
    fn multiline_fixture_fits_product_pages_and_round_trips() {
        let content = deterministic_multiline_content(3 * 1024 * 1024, "large-fixture")
            .expect("valid large fixture");
        let lines = content.split('\n').collect::<Vec<_>>();
        let pages = lines
            .chunks(usize::try_from(VERIFICATION_PAGE_LINES).expect("page size fits usize"))
            .map(|chunk| chunk.join("\n"))
            .collect::<Vec<_>>();

        assert_eq!(content.len(), 3 * 1024 * 1024);
        assert!(!content.ends_with('\n'));
        assert!(lines.iter().all(|line| line.len() < FIXTURE_LINE_BYTES));
        assert!(pages
            .iter()
            .all(|page| page.len() <= MAX_RUNTIME_READ_BYTES as usize));
        assert_eq!(pages.join("\n"), content);
    }

    #[test]
    fn edit_fixture_rejects_density_that_cannot_place_every_edit() {
        let tokens = vec![edit_token("fixture", 0, 0), edit_token("fixture", 0, 1)];
        let result = build_edit_content(OperationId::FileEdit, 128, UnitRatio(0.25), &tokens);
        assert!(matches!(
            result,
            Err(ExecutorError::InvalidFixture {
                operation: OperationId::FileEdit,
                ..
            })
        ));
    }

    #[test]
    fn blame_fixture_has_exact_line_and_segment_distribution() {
        let content = blame_content(11, 3, 7);
        let lines = content.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 11);
        assert!(lines[..4]
            .iter()
            .all(|line| line.starts_with("A|00000007|")));
        assert!(lines[4..8]
            .iter()
            .all(|line| line.starts_with("B|00000007|")));
        assert!(lines[8..]
            .iter()
            .all(|line| line.starts_with("A|00000007|")));
    }

    #[test]
    fn product_file_response_schemas_reject_unknown_fields() {
        let mut read = json!({
            "path": "fixture.txt",
            "content": "x",
            "start_line": 1,
            "num_lines": 1,
            "total_lines": 1,
            "bytes_read": 1,
            "total_bytes": 1,
            "next_offset": null,
            "truncated": false
        });
        assert!(parse_read(read.clone(), OperationId::FileRead).is_ok());
        read["unexpected"] = json!(true);
        assert!(parse_read(read, OperationId::FileRead).is_err());

        let mut write = json!({
            "type": "update",
            "path": "fixture.txt",
            "bytes_written": 1
        });
        assert!(parse_write(write.clone(), OperationId::FileWrite).is_ok());
        write["unexpected"] = json!(true);
        assert!(parse_write(write, OperationId::FileWrite).is_err());

        let mut edit = json!({
            "type": "update",
            "path": "fixture.txt",
            "edits_applied": 1,
            "replacements": 1,
            "bytes_written": 1
        });
        assert!(parse_edit(edit.clone()).is_ok());
        edit["unexpected"] = json!(true);
        assert!(parse_edit(edit).is_err());

        let mut blame = json!({
            "path": "fixture.txt",
            "ranges": [{"start_line": 1, "line_count": 1, "owner": "snapshot"}]
        });
        assert!(parse_blame(blame.clone()).is_ok());
        blame["unexpected"] = json!(true);
        assert!(parse_blame(blame).is_err());
    }
}
