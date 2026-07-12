use serde::Serialize;

use crate::executors::{command, files, layerstack, workspace};
use crate::fixtures::{
    default_workspace_profile_directory, load_workspace_profiles, FixtureError,
    WorkspaceProfileCatalog,
};
use crate::model::{
    CheckId, CleanupPolicy, ClientCohort, CountSemantics, ExecutionShape, ExpandedOperationCell,
    FactorId, FactorRole, FamilyId, IsolationPolicy, OperationComparisonIdentity, OperationId,
    PhaseCorrelationRule, PhaseId, PhaseSource, PhaseUnit, ProductAccess, ResolvedIsolationPolicy,
    SecurityClass,
};
use crate::resources::{MetricDefinition, METRICS};

pub const DEFINITION_SCHEMA_VERSION: u32 = 2;
pub const OPERATION_SEMANTIC_REVISION: u32 = 1;
pub const FACTOR_SCHEMA_REVISION: u32 = 1;
pub const COMPARISON_PROJECTION_REVISION: u32 = 1;
// `CliE2e` remains a closed schema identity and comparison discriminator, but
// this release has no CLI product adapter. Definitions must describe executable
// capability rather than silently routing that cohort through the direct client.
pub const SUPPORTED_COHORTS: &[ClientCohort] = &[ClientCohort::DirectClient];
pub const FACTOR_ROLES: &[FactorRole] = &[FactorRole::Varied, FactorRole::Controlled];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FamilyDefinition {
    pub id: FamilyId,
    pub label: &'static str,
    pub help: &'static str,
    pub research_question: &'static str,
    pub measured_boundary: &'static str,
}

pub const COMMAND_FAMILY: FamilyDefinition = FamilyDefinition {
    id: FamilyId::Command,
    label: "Command",
    help: "Bounded, compile-time command cases executed through the public runtime operation.",
    research_question: "How do concurrency, workspace scale, command work, and session lifecycle affect command execution?",
    measured_boundary: "Explicit-session cells measure command admission and execution; automatic-session cells deliberately include create, publish, and destroy lifecycle.",
};

pub const FILES_FAMILY: FamilyDefinition = FamilyDefinition {
    id: FamilyId::Files,
    label: "File Operations",
    help: "Deterministic read, write, edit, and EphemeralOS ownership workloads over published snapshots or live sessions.",
    research_question: "How do concurrency, payload, topology, source, and mutation destination affect file operations?",
    measured_boundary: "Each request measures exactly one public file operation; fixture setup, verification, and cleanup remain separately timed.",
};

pub const WORKSPACE_FAMILY: FamilyDefinition = FamilyDefinition {
    id: FamilyId::WorkspaceLifecycle,
    label: "Workspace Lifecycle",
    help: "Concurrent explicit no_op session creation through the exact internal test adapter.",
    research_question: "How do workspace scale, network profile, and concurrent session count affect time to ready?",
    measured_boundary: "A prepared sandbox and fixture precede the barrier; the measured operation creates C independent sessions and observes readiness.",
};

pub const LAYERSTACK_FAMILY: FamilyDefinition = FamilyDefinition {
    id: FamilyId::LayerStack,
    label: "LayerStack",
    help: "Destructive squash studies over fresh deterministic layer and live-session topologies.",
    research_question: "How do storage topology and live-session load affect squash, commit, and remount behavior?",
    measured_boundary: "One public manager request includes storage planning, flatten, atomic commit, and the bounded post-commit live-session remount sweep.",
};

pub const FAMILY_DEFINITIONS: &[FamilyDefinition] = &[
    COMMAND_FAMILY,
    FILES_FAMILY,
    WORKSPACE_FAMILY,
    LAYERSTACK_FAMILY,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorValueKind {
    UnsignedInteger,
    UnitRatio,
    Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorUnit {
    Count,
    Bytes,
    Ratio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCatalogId {
    WorkspaceProfiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactorConstraint {
    Positive,
    NonNegative,
    UnitInterval,
    Choices { values: &'static [&'static str] },
    ProfileCatalog { catalog: ProfileCatalogId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonParticipation {
    ScientificInvariant,
    NonScientific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FactorDefinition {
    pub id: FactorId,
    pub label: &'static str,
    pub help: &'static str,
    pub value_kind: FactorValueKind,
    pub unit: Option<FactorUnit>,
    pub constraint: FactorConstraint,
    pub comparison: ComparisonParticipation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckReference {
    pub id: CheckId,
    pub label: &'static str,
    pub help: &'static str,
    pub semantic_revision: u32,
    pub evidence_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhaseReference {
    pub id: PhaseId,
    pub label: &'static str,
    pub help: &'static str,
    pub semantic_revision: u32,
    pub unit: PhaseUnit,
    pub source: PhaseSource,
    pub correlation: PhaseCorrelationRule,
    pub trace_span_name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ComparisonProjectionDefinition {
    pub semantic_revision: u32,
    pub factors: &'static [FactorId],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperationDefinition {
    pub id: OperationId,
    pub family: FamilyId,
    pub label: &'static str,
    pub help: &'static str,
    pub measured_boundary: &'static str,
    pub count_semantics_help: &'static str,
    pub semantic_revision: u32,
    pub factor_schema_revision: u32,
    pub count_semantics: CountSemantics,
    pub execution_shape: ExecutionShape,
    pub isolation: IsolationPolicy,
    pub cleanup: CleanupPolicy,
    pub product_access: ProductAccess,
    pub supported_cohorts: &'static [ClientCohort],
    pub security_class: SecurityClass,
    pub factors: &'static [FactorDefinition],
    pub checks: &'static [CheckReference],
    pub phases: &'static [PhaseReference],
    pub comparison: ComparisonProjectionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefinitionCatalog {
    pub schema_version: u32,
    pub families: &'static [FamilyDefinition],
    pub factor_roles: &'static [FactorRole],
    pub metrics: &'static [MetricDefinition],
    pub workspace_profiles: WorkspaceProfileCatalog,
    pub operations: Vec<&'static OperationDefinition>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationComparisonKey {
    pub operation: OperationId,
    pub semantic_revision: u32,
    pub factor_schema_revision: u32,
    pub comparison_projection_revision: u32,
    pub count_semantics: CountSemantics,
    pub product_access: ProductAccess,
    pub isolation: ResolvedIsolationPolicy,
    pub identity: OperationComparisonIdentity,
}

#[must_use]
pub const fn definition(id: OperationId) -> &'static OperationDefinition {
    match id {
        OperationId::ExecCommand => &command::DEFINITION,
        OperationId::FileRead => &files::READ_DEFINITION,
        OperationId::FileWrite => &files::WRITE_DEFINITION,
        OperationId::FileEdit => &files::EDIT_DEFINITION,
        OperationId::FileBlame => &files::BLAME_DEFINITION,
        OperationId::CreateWorkspace => &workspace::DEFINITION,
        OperationId::SquashLayerstack => &layerstack::DEFINITION,
    }
}

#[must_use]
pub fn catalog() -> DefinitionCatalog {
    try_catalog().expect("versioned workspace profile defaults must be valid")
}

pub fn try_catalog() -> Result<DefinitionCatalog, FixtureError> {
    let workspace_profiles = load_workspace_profiles(&default_workspace_profile_directory())?;
    Ok(catalog_with_workspace_profiles(workspace_profiles))
}

#[must_use]
pub fn catalog_with_workspace_profiles(
    workspace_profiles: WorkspaceProfileCatalog,
) -> DefinitionCatalog {
    DefinitionCatalog {
        schema_version: DEFINITION_SCHEMA_VERSION,
        families: FAMILY_DEFINITIONS,
        factor_roles: FACTOR_ROLES,
        metrics: METRICS,
        workspace_profiles,
        operations: OperationId::ALL.into_iter().map(definition).collect(),
    }
}

#[must_use]
pub fn operation_comparison_identity(cell: &ExpandedOperationCell) -> OperationComparisonIdentity {
    match cell {
        ExpandedOperationCell::ExecCommand(cell) => {
            OperationComparisonIdentity::ExecCommand(command::comparison_identity(cell))
        }
        ExpandedOperationCell::FileRead(cell) => {
            OperationComparisonIdentity::FileRead(files::read_comparison_identity(cell))
        }
        ExpandedOperationCell::FileWrite(cell) => {
            OperationComparisonIdentity::FileWrite(files::write_comparison_identity(cell))
        }
        ExpandedOperationCell::FileEdit(cell) => {
            OperationComparisonIdentity::FileEdit(files::edit_comparison_identity(cell))
        }
        ExpandedOperationCell::FileBlame(cell) => {
            OperationComparisonIdentity::FileBlame(files::blame_comparison_identity(cell))
        }
        ExpandedOperationCell::CreateWorkspace(cell) => {
            OperationComparisonIdentity::CreateWorkspace(workspace::comparison_identity(cell))
        }
        ExpandedOperationCell::SquashLayerstack(cell) => {
            OperationComparisonIdentity::SquashLayerstack(layerstack::comparison_identity(cell))
        }
    }
}

#[must_use]
pub fn operation_comparison_key(cell: &ExpandedOperationCell) -> OperationComparisonKey {
    let definition = definition(cell.id());
    OperationComparisonKey {
        operation: definition.id,
        semantic_revision: definition.semantic_revision,
        factor_schema_revision: definition.factor_schema_revision,
        comparison_projection_revision: definition.comparison.semantic_revision,
        count_semantics: definition.count_semantics,
        product_access: definition.product_access,
        isolation: cell.resolved_isolation(),
        identity: operation_comparison_identity(cell),
    }
}
