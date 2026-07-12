use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::executors::{command, files, layerstack, workspace};

/// Canonical wire identity for a closed categorical experimental factor.
///
/// Implementations live in the shared typed model so artifact/report consumers
/// do not need to depend on operation implementation modules merely to project
/// a persisted factor level.
pub trait CanonicalFactorLevel: Copy {
    fn canonical_factor_level(self) -> &'static str;
}

impl CanonicalFactorLevel for command::CommandSessionMode {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Automatic => "automatic",
        }
    }
}

impl CanonicalFactorLevel for command::CommandCase {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Noop => "noop",
            Self::Output64Kib => "output64_kib",
            Self::Cpu50Ms => "cpu50_ms",
            Self::FixtureRead => "fixture_read",
        }
    }
}

impl CanonicalFactorLevel for files::FileReadSource {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Session => "session",
        }
    }
}

impl CanonicalFactorLevel for files::TargetMode {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::SameTarget => "same_target",
        }
    }
}

impl CanonicalFactorLevel for files::MutationDestination {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyId {
    Command,
    Files,
    WorkspaceLifecycle,
    LayerStack,
}

impl FamilyId {
    pub const ALL: [Self; 4] = [
        Self::Command,
        Self::Files,
        Self::WorkspaceLifecycle,
        Self::LayerStack,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationId {
    ExecCommand,
    FileRead,
    FileWrite,
    FileEdit,
    FileBlame,
    CreateWorkspace,
    SquashLayerstack,
}

impl OperationId {
    pub const ALL: [Self; 7] = [
        Self::ExecCommand,
        Self::FileRead,
        Self::FileWrite,
        Self::FileEdit,
        Self::FileBlame,
        Self::CreateWorkspace,
        Self::SquashLayerstack,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientCohort {
    DirectClient,
    CliE2e,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationScope {
    All,
    Command,
    Files,
    Workspace,
    #[serde(rename = "layerstack")]
    LayerStack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationBase {
    pub id: String,
    pub version: u32,
    pub scope: ConfigurationScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageReference(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentPlan {
    pub image: ImageReference,
    pub client_cohort: ClientCohort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellOrder {
    RandomizedBlocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialCountPlan {
    pub warmups: u32,
    pub measured_trials: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialDefaultsPlan {
    pub fast: TrialCountPlan,
    pub destructive: TrialCountPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutPlan {
    pub default: u64,
    pub squash_layerstack: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolPlan {
    pub order: CellOrder,
    pub resource_interval_ms: u64,
    pub trial_defaults: TrialDefaultsPlan,
    pub timeout_ms: TimeoutPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreatmentField {
    SourceCommit,
    SourceDiffHash,
    DaemonBinaryHash,
    GatewayBinaryHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonPlan {
    pub protocol_id: String,
    pub protocol_version: u32,
    pub treatment_fields: BTreeSet<TreatmentField>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentPlan {
    pub schema_version: u32,
    pub name: String,
    pub configuration_base: ConfigurationBase,
    pub seed: u64,
    pub environment: EnvironmentPlan,
    pub protocol: ProtocolPlan,
    pub operations: Vec<OperationPlan>,
    pub comparison: Option<ComparisonPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorRole {
    Varied,
    Controlled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Factor<T> {
    pub role: FactorRole,
    pub values: Vec<T>,
    pub control: Option<T>,
}

/// Closed, typed value used only to project authoritative operation factors
/// into generic tables and exports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NormalizedFactorValue {
    UnsignedInteger(u64),
    Ratio(f64),
    Choice(String),
}

/// Stable generic projection derived from one typed operation plan and one of
/// its typed expanded cells. It is never an execution or validation authority.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedFactorProjection {
    pub id: FactorId,
    pub role: FactorRole,
    pub value: NormalizedFactorValue,
    pub control: Option<NormalizedFactorValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorProjectionError {
    OperationMismatch {
        plan: OperationId,
        cell: OperationId,
    },
    ShapeMismatch {
        operation: OperationId,
    },
}

impl fmt::Display for FactorProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationMismatch { plan, cell } => {
                write!(
                    formatter,
                    "plan operation {plan:?} does not match cell operation {cell:?}"
                )
            }
            Self::ShapeMismatch { operation } => {
                write!(
                    formatter,
                    "factor projection shape is invalid for {operation:?}"
                )
            }
        }
    }
}

impl std::error::Error for FactorProjectionError {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnitRatio(pub f64);

impl UnitRatio {
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.0.is_finite() && (0.0..=1.0).contains(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceProfileId(String);

impl WorkspaceProfileId {
    pub const MAX_BYTES: usize = 64;

    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceProfileIdError> {
        let value = value.into();
        if value.len() > Self::MAX_BYTES
            || !value.split('_').all(|part| {
                let mut bytes = part.bytes();
                bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                    && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        {
            return Err(WorkspaceProfileIdError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceProfileId {
    type Err = WorkspaceProfileIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for WorkspaceProfileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorkspaceProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceProfileIdError;

impl fmt::Display for WorkspaceProfileIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "workspace profile id must be 1 to {} bytes of lowercase snake_case",
            WorkspaceProfileId::MAX_BYTES
        )
    }
}

impl std::error::Error for WorkspaceProfileIdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedNetworkProfile {
    Shared,
    Isolated,
}

impl CanonicalFactorLevel for AllowedNetworkProfile {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Isolated => "isolated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivity {
    Idle,
    Active,
}

impl CanonicalFactorLevel for SessionActivity {
    fn canonical_factor_level(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "configuration",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperationPlan {
    ExecCommand(command::ExecCommandPlan),
    FileRead(files::FileReadPlan),
    FileWrite(files::FileWritePlan),
    FileEdit(files::FileEditPlan),
    FileBlame(files::FileBlamePlan),
    CreateWorkspace(workspace::CreateWorkspacePlan),
    SquashLayerstack(layerstack::SquashLayerstackPlan),
}

impl OperationPlan {
    #[must_use]
    pub const fn id(&self) -> OperationId {
        match self {
            Self::ExecCommand(_) => OperationId::ExecCommand,
            Self::FileRead(_) => OperationId::FileRead,
            Self::FileWrite(_) => OperationId::FileWrite,
            Self::FileEdit(_) => OperationId::FileEdit,
            Self::FileBlame(_) => OperationId::FileBlame,
            Self::CreateWorkspace(_) => OperationId::CreateWorkspace,
            Self::SquashLayerstack(_) => OperationId::SquashLayerstack,
        }
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        match self {
            Self::ExecCommand(plan) => plan.enabled,
            Self::FileRead(plan) => plan.enabled,
            Self::FileWrite(plan) => plan.enabled,
            Self::FileEdit(plan) => plan.enabled,
            Self::FileBlame(plan) => plan.enabled,
            Self::CreateWorkspace(plan) => plan.enabled,
            Self::SquashLayerstack(plan) => plan.enabled,
        }
    }

    fn normalized_factor_design(
        &self,
    ) -> Vec<(FactorId, FactorRole, Option<NormalizedFactorValue>)> {
        match self {
            Self::ExecCommand(plan) => vec![
                project_factor(
                    FactorId::ConcurrentRequests,
                    &plan.factors.concurrent_requests,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::WorkspaceProfile,
                    &plan.factors.workspace_profile,
                    normalized_workspace_profile,
                ),
                project_factor(
                    FactorId::SessionMode,
                    &plan.factors.session_mode,
                    normalized_factor_level,
                ),
                project_factor(
                    FactorId::CommandCase,
                    &plan.factors.command_case,
                    normalized_factor_level,
                ),
            ],
            Self::FileRead(plan) => vec![
                project_factor(
                    FactorId::ConcurrentRequests,
                    &plan.factors.concurrent_requests,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::ReturnedBytes,
                    &plan.factors.returned_bytes,
                    normalized_u64,
                ),
                project_factor(
                    FactorId::ReadSource,
                    &plan.factors.source,
                    normalized_factor_level,
                ),
                project_factor(
                    FactorId::TargetMode,
                    &plan.factors.target_mode,
                    normalized_factor_level,
                ),
            ],
            Self::FileWrite(plan) => vec![
                project_factor(
                    FactorId::ConcurrentRequests,
                    &plan.factors.concurrent_requests,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::ContentBytes,
                    &plan.factors.content_bytes,
                    normalized_u64,
                ),
                project_factor(
                    FactorId::MutationDestination,
                    &plan.factors.destination,
                    normalized_factor_level,
                ),
                project_factor(
                    FactorId::TargetMode,
                    &plan.factors.target_mode,
                    normalized_factor_level,
                ),
            ],
            Self::FileEdit(plan) => vec![
                project_factor(
                    FactorId::ConcurrentRequests,
                    &plan.factors.concurrent_requests,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::FileBytes,
                    &plan.factors.file_bytes,
                    normalized_u64,
                ),
                project_factor(
                    FactorId::ReplacementCount,
                    &plan.factors.replacement_count,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::MatchDensity,
                    &plan.factors.match_density,
                    normalized_ratio,
                ),
                project_factor(
                    FactorId::MutationDestination,
                    &plan.factors.destination,
                    normalized_factor_level,
                ),
                project_factor(
                    FactorId::TargetMode,
                    &plan.factors.target_mode,
                    normalized_factor_level,
                ),
            ],
            Self::FileBlame(plan) => vec![
                project_factor(
                    FactorId::ConcurrentRequests,
                    &plan.factors.concurrent_requests,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::LineCount,
                    &plan.factors.line_count,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::OwnershipSegments,
                    &plan.factors.ownership_segments,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::AuditabilityEventCount,
                    &plan.factors.auditability_event_count,
                    normalized_u32,
                ),
            ],
            Self::CreateWorkspace(plan) => vec![
                project_factor(
                    FactorId::WorkspaceCount,
                    &plan.factors.workspace_count,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::WorkspaceProfile,
                    &plan.factors.workspace_profile,
                    normalized_workspace_profile,
                ),
                project_factor(
                    FactorId::NetworkProfile,
                    &plan.factors.network_profile,
                    normalized_factor_level,
                ),
            ],
            Self::SquashLayerstack(plan) => vec![
                project_factor(
                    FactorId::LiveSessions,
                    &plan.factors.live_sessions,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::RequestedMigrationRatio,
                    &plan.factors.requested_migration_ratio,
                    normalized_ratio,
                ),
                project_factor(
                    FactorId::RemountParallelism,
                    &plan.factors.remount_parallelism,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::SquashableBlocks,
                    &plan.factors.squashable_blocks,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::LayersPerBlock,
                    &plan.factors.layers_per_block,
                    normalized_u32,
                ),
                project_factor(
                    FactorId::PayloadBytes,
                    &plan.factors.payload_bytes,
                    normalized_u64,
                ),
                project_factor(
                    FactorId::SessionActivity,
                    &plan.factors.session_activity,
                    normalized_factor_level,
                ),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "cell",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExpandedOperationCell {
    ExecCommand(command::ExecCommandCell),
    FileRead(files::FileReadCell),
    FileWrite(files::FileWriteCell),
    FileEdit(files::FileEditCell),
    FileBlame(files::FileBlameCell),
    CreateWorkspace(workspace::CreateWorkspaceCell),
    SquashLayerstack(layerstack::SquashLayerstackCell),
}

impl ExpandedOperationCell {
    #[must_use]
    pub const fn id(&self) -> OperationId {
        match self {
            Self::ExecCommand(_) => OperationId::ExecCommand,
            Self::FileRead(_) => OperationId::FileRead,
            Self::FileWrite(_) => OperationId::FileWrite,
            Self::FileEdit(_) => OperationId::FileEdit,
            Self::FileBlame(_) => OperationId::FileBlame,
            Self::CreateWorkspace(_) => OperationId::CreateWorkspace,
            Self::SquashLayerstack(_) => OperationId::SquashLayerstack,
        }
    }

    #[must_use]
    pub const fn measured_invocation_count(&self) -> u32 {
        match self {
            Self::ExecCommand(cell) => cell.concurrent_requests,
            Self::FileRead(cell) => cell.concurrent_requests,
            Self::FileWrite(cell) => cell.concurrent_requests,
            Self::FileEdit(cell) => cell.concurrent_requests,
            Self::FileBlame(cell) => cell.concurrent_requests,
            Self::CreateWorkspace(cell) => cell.workspace_count,
            Self::SquashLayerstack(_) => 1,
        }
    }

    #[must_use]
    pub const fn resolved_isolation(&self) -> ResolvedIsolationPolicy {
        match self {
            Self::ExecCommand(cell) => cell.resolved_isolation,
            Self::FileRead(cell) => cell.resolved_isolation,
            Self::FileWrite(cell) => cell.resolved_isolation,
            Self::FileEdit(cell) => cell.resolved_isolation,
            Self::FileBlame(cell) => cell.resolved_isolation,
            Self::CreateWorkspace(cell) => cell.resolved_isolation,
            Self::SquashLayerstack(cell) => cell.resolved_isolation,
        }
    }

    /// Derives the generic factor view from the authoritative typed plan and
    /// expanded cell. Operation-specific matching remains at this closed model
    /// boundary so report/statistics/compare code does not change when an
    /// operation is added.
    pub fn normalized_factor_projection(
        &self,
        plan: &OperationPlan,
    ) -> Result<Vec<NormalizedFactorProjection>, FactorProjectionError> {
        if plan.id() != self.id() {
            return Err(FactorProjectionError::OperationMismatch {
                plan: plan.id(),
                cell: self.id(),
            });
        }
        let design = plan.normalized_factor_design();
        let values = self.normalized_factor_values();
        if design.len() != values.len() {
            return Err(FactorProjectionError::ShapeMismatch {
                operation: self.id(),
            });
        }
        design
            .into_iter()
            .zip(values)
            .map(|((design_id, role, control), (value_id, value))| {
                if design_id != value_id {
                    return Err(FactorProjectionError::ShapeMismatch {
                        operation: self.id(),
                    });
                }
                Ok(NormalizedFactorProjection {
                    id: design_id,
                    role,
                    value,
                    control,
                })
            })
            .collect()
    }

    fn normalized_factor_values(&self) -> Vec<(FactorId, NormalizedFactorValue)> {
        match self {
            Self::ExecCommand(cell) => vec![
                (
                    FactorId::ConcurrentRequests,
                    normalized_u32(&cell.concurrent_requests),
                ),
                (
                    FactorId::WorkspaceProfile,
                    normalized_workspace_profile(&cell.workspace_profile),
                ),
                (
                    FactorId::SessionMode,
                    normalized_factor_level(&cell.session_mode),
                ),
                (
                    FactorId::CommandCase,
                    normalized_factor_level(&cell.command_case),
                ),
            ],
            Self::FileRead(cell) => vec![
                (
                    FactorId::ConcurrentRequests,
                    normalized_u32(&cell.concurrent_requests),
                ),
                (
                    FactorId::ReturnedBytes,
                    normalized_u64(&cell.returned_bytes),
                ),
                (FactorId::ReadSource, normalized_factor_level(&cell.source)),
                (
                    FactorId::TargetMode,
                    normalized_factor_level(&cell.target_mode),
                ),
            ],
            Self::FileWrite(cell) => vec![
                (
                    FactorId::ConcurrentRequests,
                    normalized_u32(&cell.concurrent_requests),
                ),
                (FactorId::ContentBytes, normalized_u64(&cell.content_bytes)),
                (
                    FactorId::MutationDestination,
                    normalized_factor_level(&cell.destination),
                ),
                (
                    FactorId::TargetMode,
                    normalized_factor_level(&cell.target_mode),
                ),
            ],
            Self::FileEdit(cell) => vec![
                (
                    FactorId::ConcurrentRequests,
                    normalized_u32(&cell.concurrent_requests),
                ),
                (FactorId::FileBytes, normalized_u64(&cell.file_bytes)),
                (
                    FactorId::ReplacementCount,
                    normalized_u32(&cell.replacement_count),
                ),
                (
                    FactorId::MatchDensity,
                    normalized_ratio(&cell.match_density),
                ),
                (
                    FactorId::MutationDestination,
                    normalized_factor_level(&cell.destination),
                ),
                (
                    FactorId::TargetMode,
                    normalized_factor_level(&cell.target_mode),
                ),
            ],
            Self::FileBlame(cell) => vec![
                (
                    FactorId::ConcurrentRequests,
                    normalized_u32(&cell.concurrent_requests),
                ),
                (FactorId::LineCount, normalized_u32(&cell.line_count)),
                (
                    FactorId::OwnershipSegments,
                    normalized_u32(&cell.ownership_segments),
                ),
                (
                    FactorId::AuditabilityEventCount,
                    normalized_u32(&cell.auditability_event_count),
                ),
            ],
            Self::CreateWorkspace(cell) => vec![
                (
                    FactorId::WorkspaceCount,
                    normalized_u32(&cell.workspace_count),
                ),
                (
                    FactorId::WorkspaceProfile,
                    normalized_workspace_profile(&cell.workspace_profile),
                ),
                (
                    FactorId::NetworkProfile,
                    normalized_factor_level(&cell.network_profile),
                ),
            ],
            Self::SquashLayerstack(cell) => vec![
                (FactorId::LiveSessions, normalized_u32(&cell.live_sessions)),
                (
                    FactorId::RequestedMigrationRatio,
                    normalized_ratio(&cell.requested_migration_ratio),
                ),
                (
                    FactorId::RemountParallelism,
                    normalized_u32(&cell.remount_parallelism),
                ),
                (
                    FactorId::SquashableBlocks,
                    normalized_u32(&cell.squashable_blocks),
                ),
                (
                    FactorId::LayersPerBlock,
                    normalized_u32(&cell.layers_per_block),
                ),
                (FactorId::PayloadBytes, normalized_u64(&cell.payload_bytes)),
                (
                    FactorId::SessionActivity,
                    normalized_factor_level(&cell.session_activity),
                ),
            ],
        }
    }
}

fn project_factor<T>(
    id: FactorId,
    factor: &Factor<T>,
    normalize: fn(&T) -> NormalizedFactorValue,
) -> (FactorId, FactorRole, Option<NormalizedFactorValue>) {
    (id, factor.role, factor.control.as_ref().map(normalize))
}

fn normalized_u32(value: &u32) -> NormalizedFactorValue {
    NormalizedFactorValue::UnsignedInteger(u64::from(*value))
}

fn normalized_u64(value: &u64) -> NormalizedFactorValue {
    NormalizedFactorValue::UnsignedInteger(*value)
}

fn normalized_ratio(value: &UnitRatio) -> NormalizedFactorValue {
    NormalizedFactorValue::Ratio(value.0)
}

fn normalized_workspace_profile(value: &WorkspaceProfileId) -> NormalizedFactorValue {
    NormalizedFactorValue::Choice(value.to_string())
}

fn normalized_factor_level<T: CanonicalFactorLevel>(value: &T) -> NormalizedFactorValue {
    NormalizedFactorValue::Choice((*value).canonical_factor_level().to_owned())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "evidence",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperationEvidence {
    ExecCommand(command::ExecCommandEvidence),
    FileRead(files::FileReadEvidence),
    FileWrite(files::FileWriteEvidence),
    FileEdit(files::FileEditEvidence),
    FileBlame(files::FileBlameEvidence),
    CreateWorkspace(workspace::CreateWorkspaceEvidence),
    SquashLayerstack(Box<layerstack::SquashLayerstackEvidence>),
}

impl OperationEvidence {
    #[must_use]
    pub const fn id(&self) -> OperationId {
        match self {
            Self::ExecCommand(_) => OperationId::ExecCommand,
            Self::FileRead(_) => OperationId::FileRead,
            Self::FileWrite(_) => OperationId::FileWrite,
            Self::FileEdit(_) => OperationId::FileEdit,
            Self::FileBlame(_) => OperationId::FileBlame,
            Self::CreateWorkspace(_) => OperationId::CreateWorkspace,
            Self::SquashLayerstack(_) => OperationId::SquashLayerstack,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "identity",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperationComparisonIdentity {
    ExecCommand(command::ExecCommandComparisonIdentity),
    FileRead(files::FileReadComparisonIdentity),
    FileWrite(files::FileWriteComparisonIdentity),
    FileEdit(files::FileEditComparisonIdentity),
    FileBlame(files::FileBlameComparisonIdentity),
    CreateWorkspace(workspace::CreateWorkspaceComparisonIdentity),
    SquashLayerstack(layerstack::SquashLayerstackComparisonIdentity),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductOperation {
    ExecCommand,
    FileRead,
    FileWrite,
    FileEdit,
    FileBlame,
    SquashLayerstacks,
}

impl ProductOperation {
    #[must_use]
    pub const fn catalog_spec(self) -> &'static sandbox_operation_contract::OperationSpec {
        match self {
            Self::ExecCommand => &sandbox_operation_catalog::runtime::EXEC_COMMAND_SPEC,
            Self::FileRead => &sandbox_operation_catalog::runtime::FILE_READ_SPEC,
            Self::FileWrite => &sandbox_operation_catalog::runtime::FILE_WRITE_SPEC,
            Self::FileEdit => &sandbox_operation_catalog::runtime::FILE_EDIT_SPEC,
            Self::FileBlame => &sandbox_operation_catalog::runtime::FILE_BLAME_SPEC,
            Self::SquashLayerstacks => &sandbox_operation_catalog::manager::SQUASH_LAYERSTACKS_SPEC,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonHttpAction {
    FileList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAction {
    CreateNoOpSession,
    DestroySession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "action", rename_all = "snake_case")]
pub enum ProductAccess {
    PublicGateway(ProductOperation),
    DaemonHttp(DaemonHttpAction),
    InternalWorkspace(WorkspaceAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CountSemantics {
    ConcurrentRequests { factor: FactorId },
    ConcurrentWorkspaceCreates { factor: FactorId },
    SingleRequestWithPreparedLoad { load_factor: FactorId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionShape {
    BarrierRequestBatch,
    BarrierWorkspaceCreation,
    SingleRequestAfterPreparedLoad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationPolicy {
    SessionModeDependent,
    ReusableVerifiedFixture,
    MutationDestinationDependent,
    PreparedSandboxPerCell,
    FreshTopologyPerTrial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedIsolationPolicy {
    ReusableVerifiedFixture,
    FreshSessionsPerTrial,
    FreshSandboxPerTrial,
    PreparedSandboxPerCell,
    FreshTopologyPerTrial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    VerifyFixtureUnchanged,
    ResolveFromIsolation,
    DestroySessionsAndVerifyBaseline,
    DestroyTopologyAndVerifyBaseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityClass {
    BoundedShell,
    PublicReadOnly,
    PublicMutation,
    InternalWorkspaceLifecycle,
    DestructiveManagerMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorId {
    ConcurrentRequests,
    WorkspaceProfile,
    SessionMode,
    CommandCase,
    ReturnedBytes,
    ReadSource,
    TargetMode,
    ContentBytes,
    MutationDestination,
    FileBytes,
    ReplacementCount,
    MatchDensity,
    LineCount,
    OwnershipSegments,
    AuditabilityEventCount,
    WorkspaceCount,
    NetworkProfile,
    LiveSessions,
    RequestedMigrationRatio,
    RemountParallelism,
    SquashableBlocks,
    LayersPerBlock,
    PayloadBytes,
    SessionActivity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    CommandExitStatus,
    CommandOutput,
    CommandLifecycle,
    FileReadWindow,
    FileContentHash,
    MutationAttribution,
    FileEditReplacementCount,
    BlameRangeCoverage,
    BlameOwnership,
    WorkspaceReady,
    WorkspaceNetworkProfile,
    WorkspaceRegistryBaseline,
    LayerstackContentEquivalence,
    LayerstackManifestReduction,
    LayerstackDispositionAccounting,
    LayerstackSessionUsability,
    LayerstackResidue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseId {
    LayerstackSquash,
    LayerstackStoragePlan,
    LayerstackFlatten,
    LayerstackCommit,
    LayerstackRemountSweep,
    WorkspaceSessionRemount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseSource {
    ProductTrace,
}

/// Unit owned by a semantic phase definition. Phase observations are
/// normalized to this unit before they are persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseUnit {
    Nanoseconds,
}

/// Closed rule used to correlate product telemetry with a benchmark request.
/// The registered span name is stored alongside this rule in definitions and
/// observations; arbitrary span names never become executable behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseCorrelationRule {
    ExactRequestTraceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorViolation {
    ControlledValueCount,
    ControlledHasControl,
    VariedValueCount,
    VariedMissingControl,
    ControlNotInValues,
    DuplicateValues,
    ZeroNotAllowed,
    OutOfUnitInterval,
    IncompatibleCombination,
    SafetyBoundExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationValidationError {
    pub operation: OperationId,
    pub factor: FactorId,
    pub violation: FactorViolation,
}

pub(crate) fn validate_factor<T: PartialEq>(
    operation: OperationId,
    id: FactorId,
    factor: &Factor<T>,
) -> Vec<OperationValidationError> {
    let mut errors = Vec::new();
    match factor.role {
        FactorRole::Controlled => {
            if factor.values.len() != 1 {
                errors.push(validation_error(
                    operation,
                    id,
                    FactorViolation::ControlledValueCount,
                ));
            }
            if factor.control.is_some() {
                errors.push(validation_error(
                    operation,
                    id,
                    FactorViolation::ControlledHasControl,
                ));
            }
        }
        FactorRole::Varied => {
            if factor.values.len() < 2 {
                errors.push(validation_error(
                    operation,
                    id,
                    FactorViolation::VariedValueCount,
                ));
            }
            match &factor.control {
                None => errors.push(validation_error(
                    operation,
                    id,
                    FactorViolation::VariedMissingControl,
                )),
                Some(control) if !factor.values.contains(control) => errors.push(validation_error(
                    operation,
                    id,
                    FactorViolation::ControlNotInValues,
                )),
                Some(_) => {}
            }
        }
    }
    if factor
        .values
        .iter()
        .enumerate()
        .any(|(index, value)| factor.values[..index].contains(value))
    {
        errors.push(validation_error(
            operation,
            id,
            FactorViolation::DuplicateValues,
        ));
    }
    errors
}

pub(crate) fn validate_nonzero_u32(
    operation: OperationId,
    id: FactorId,
    factor: &Factor<u32>,
) -> Vec<OperationValidationError> {
    if factor.values.contains(&0) {
        vec![validation_error(
            operation,
            id,
            FactorViolation::ZeroNotAllowed,
        )]
    } else {
        Vec::new()
    }
}

pub(crate) fn validate_nonzero_u64(
    operation: OperationId,
    id: FactorId,
    factor: &Factor<u64>,
) -> Vec<OperationValidationError> {
    if factor.values.contains(&0) {
        vec![validation_error(
            operation,
            id,
            FactorViolation::ZeroNotAllowed,
        )]
    } else {
        Vec::new()
    }
}

pub(crate) fn validate_unit_ratio(
    operation: OperationId,
    id: FactorId,
    factor: &Factor<UnitRatio>,
) -> Vec<OperationValidationError> {
    if factor.values.iter().any(|value| !value.is_valid()) {
        vec![validation_error(
            operation,
            id,
            FactorViolation::OutOfUnitInterval,
        )]
    } else {
        Vec::new()
    }
}

const fn validation_error(
    operation: OperationId,
    factor: FactorId,
    violation: FactorViolation,
) -> OperationValidationError {
    OperationValidationError {
        operation,
        factor,
        violation,
    }
}
