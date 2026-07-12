use std::{sync::Mutex, time::Instant};

use serde::{Deserialize, Serialize};

use crate::daemon_session::{CreatedSession, WorkspaceSessionLifecycle};

use crate::definitions::{
    CheckReference, ComparisonParticipation, ComparisonProjectionDefinition, FactorConstraint,
    FactorDefinition, FactorUnit, FactorValueKind, OperationDefinition, ProfileCatalogId,
    FACTOR_SCHEMA_REVISION, OPERATION_SEMANTIC_REVISION, SUPPORTED_COHORTS,
};
use crate::model::{
    validate_factor, validate_nonzero_u32, AllowedNetworkProfile, CheckId, CleanupPolicy,
    CountSemantics, ExecutionShape, Factor, FactorId, FamilyId, IsolationPolicy, OperationEvidence,
    OperationId, OperationValidationError, ProductAccess, ResolvedIsolationPolicy, SecurityClass,
    WorkspaceAction, WorkspaceProfileId,
};

use super::command::probe_session;
use super::{
    check_result, register_session, session_registry, teardown_registered_sessions, ExecutorError,
    InvocationOutcome, OperationLifecycle, ProductOutputStatus, ResponseMetadata, RuntimeContext,
    RuntimeInvocation, RuntimeOutput, SessionRegistry, TeardownResult, Verification,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceFactors {
    pub workspace_count: Factor<u32>,
    pub workspace_profile: Factor<WorkspaceProfileId>,
    pub network_profile: Factor<AllowedNetworkProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspacePlan {
    pub enabled: bool,
    pub factors: CreateWorkspaceFactors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceCell {
    pub workspace_count: u32,
    pub workspace_profile: WorkspaceProfileId,
    pub network_profile: AllowedNetworkProfile,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceEvidence {
    pub requested_count: u32,
    pub created_count: u32,
    pub ready_count: u32,
    pub destroyed_count: u32,
    pub network_profile_matches: u32,
    pub registry_baseline_restored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceComparisonIdentity {
    pub workspace_count: u32,
    pub workspace_profile: WorkspaceProfileId,
    pub network_profile: AllowedNetworkProfile,
}

const FACTORS: &[FactorDefinition] = &[
    FactorDefinition {
        id: FactorId::WorkspaceCount,
        label: "Concurrent workspace creations",
        help: "Number of independent create_workspace_session requests released from one barrier.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::WorkspaceProfile,
        label: "Workspace profile",
        help: "Deterministic materialized fixture scale used by every requested session.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::ProfileCatalog {
            catalog: ProfileCatalogId::WorkspaceProfiles,
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::NetworkProfile,
        label: "Network profile",
        help: "Allowlisted network mode applied to each created workspace session.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["shared", "isolated"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
];

const CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::WorkspaceReady,
        label: "Workspace ready",
        help: "Every successful creation reaches the product's ready state.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
    CheckReference {
        id: CheckId::WorkspaceNetworkProfile,
        label: "Network profile",
        help: "The created sessions expose the requested allowlisted network profile.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
    CheckReference {
        id: CheckId::WorkspaceRegistryBaseline,
        label: "Workspace registry baseline",
        help: "Paired destruction restores the pre-trial workspace-session registry baseline.",
        semantic_revision: 1,
        evidence_limit: 32,
    },
];

const COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::WorkspaceCount,
    FactorId::WorkspaceProfile,
    FactorId::NetworkProfile,
];

pub const DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::CreateWorkspace,
    family: FamilyId::WorkspaceLifecycle,
    label: "Create workspace",
    help: "Benchmarks the exact allowlisted create_workspace_session test adapter for explicit no_op sessions; it is not a public operation.",
    measured_boundary: "C independent session-creation requests start together after one sandbox and fixture are prepared; readiness is measured, while residual cleanup is not.",
    count_semantics_help: "Workspace count C is the number of independent product requests in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentWorkspaceCreates {
        factor: FactorId::WorkspaceCount,
    },
    execution_shape: ExecutionShape::BarrierWorkspaceCreation,
    isolation: IsolationPolicy::PreparedSandboxPerCell,
    cleanup: CleanupPolicy::DestroySessionsAndVerifyBaseline,
    product_access: ProductAccess::InternalWorkspace(WorkspaceAction::CreateNoOpSession),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::InternalWorkspaceLifecycle,
    factors: FACTORS,
    checks: CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: COMPARISON_FACTORS,
    },
};

#[must_use]
pub fn validate(plan: &CreateWorkspacePlan) -> Vec<OperationValidationError> {
    let operation = OperationId::CreateWorkspace;
    let factors = &plan.factors;
    let mut errors = validate_factor(
        operation,
        FactorId::WorkspaceCount,
        &factors.workspace_count,
    );
    errors.extend(validate_nonzero_u32(
        operation,
        FactorId::WorkspaceCount,
        &factors.workspace_count,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::WorkspaceProfile,
        &factors.workspace_profile,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::NetworkProfile,
        &factors.network_profile,
    ));
    errors
}

pub fn expand(
    plan: &CreateWorkspacePlan,
) -> Result<Vec<CreateWorkspaceCell>, Vec<OperationValidationError>> {
    let errors = validate(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }

    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &workspace_count in &factors.workspace_count.values {
        for workspace_profile in &factors.workspace_profile.values {
            for &network_profile in &factors.network_profile.values {
                cells.push(CreateWorkspaceCell {
                    workspace_count,
                    workspace_profile: workspace_profile.clone(),
                    network_profile,
                    resolved_isolation: ResolvedIsolationPolicy::PreparedSandboxPerCell,
                });
            }
        }
    }
    Ok(cells)
}

#[must_use]
pub fn comparison_identity(cell: &CreateWorkspaceCell) -> CreateWorkspaceComparisonIdentity {
    CreateWorkspaceComparisonIdentity {
        workspace_count: cell.workspace_count,
        workspace_profile: cell.workspace_profile.clone(),
        network_profile: cell.network_profile,
    }
}

#[derive(Debug)]
pub struct CreateWorkspaceRuntime;

#[derive(Debug)]
pub struct PreparedCreateWorkspace {
    sessions: SessionRegistry,
    session_baseline: usize,
    observed: Mutex<Option<WorkspaceObserved>>,
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceObserved {
    ready_count: u32,
    network_profile_matches: u32,
}

#[derive(Debug, Clone)]
pub struct CreateWorkspaceInvocation {
    request_id: String,
    network_profile: AllowedNetworkProfile,
    sessions: SessionRegistry,
}

impl RuntimeInvocation for CreateWorkspaceInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone)]
pub struct CreateWorkspaceOutput {
    pub session: CreatedSession,
    pub metadata: ResponseMetadata,
}

impl RuntimeOutput for CreateWorkspaceOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

impl OperationLifecycle for CreateWorkspaceRuntime {
    type Cell = CreateWorkspaceCell;
    type Prepared = PreparedCreateWorkspace;
    type Invocation = CreateWorkspaceInvocation;
    type Output = CreateWorkspaceOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        if cell.workspace_count == 0 {
            return Err(ExecutorError::InvalidFixture {
                operation: OperationId::CreateWorkspace,
                reason: "workspace_count must be positive",
            });
        }
        Ok(PreparedCreateWorkspace {
            sessions: session_registry(),
            session_baseline: context.workspace_sessions().owned_session_count()?,
            observed: Mutex::new(None),
        })
    }

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        Ok((0..cell.workspace_count)
            .map(|index| CreateWorkspaceInvocation {
                request_id: format!("workspace-{index}"),
                network_profile: cell.network_profile,
                sessions: prepared.sessions.clone(),
            })
            .collect())
    }

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output> {
        let request_id = invocation.request_id;
        let result = async {
            let session = context
                .workspace_sessions()
                .create_no_op(
                    context.sandbox_id().clone(),
                    invocation.network_profile,
                    context.correlation(&request_id)?,
                )
                .await?;
            register_session(&invocation.sessions, session.clone())?;
            let projection = serde_json::json!({
                "workspace_session_id": session.workspace_session_id().as_str(),
                "network_profile": session.network_profile(),
                "finalize_policy": "no_op",
            });
            let metadata = super::response_metadata(&projection, ProductOutputStatus::Succeeded)?;
            Ok(CreateWorkspaceOutput { session, metadata })
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
        require_count(cell.workspace_count, outcomes.len())?;
        let mut checks = Vec::with_capacity(outcomes.len().saturating_mul(2));
        let mut ready_count = 0_u32;
        let mut network_profile_matches = 0_u32;
        for (index, outcome) in outcomes.iter().enumerate() {
            let started = Instant::now();
            let (ready, ready_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => {
                    match probe_session(
                        context,
                        output.session.workspace_session_id(),
                        &format!("workspace-ready-{index}"),
                    )
                    .await
                    {
                        Ok(()) => (true, "ready_and_usable".to_owned()),
                        Err(error) => (false, error.to_string()),
                    }
                }
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            if ready {
                ready_count = ready_count.saturating_add(1);
            }
            checks.push(check_result(
                context,
                OperationId::CreateWorkspace,
                CheckId::WorkspaceReady,
                Some(outcome.request_id().to_owned()),
                ready,
                "ready_and_usable",
                ready_actual,
                started,
            ));

            let started = Instant::now();
            let (network_matches, network_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => (
                    output.session.network_profile() == cell.network_profile,
                    format!("{:?}", output.session.network_profile()),
                ),
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            if network_matches {
                network_profile_matches = network_profile_matches.saturating_add(1);
            }
            checks.push(check_result(
                context,
                OperationId::CreateWorkspace,
                CheckId::WorkspaceNetworkProfile,
                Some(outcome.request_id().to_owned()),
                network_matches,
                format!("{:?}", cell.network_profile),
                network_actual,
                started,
            ));
        }
        *prepared
            .observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)? = Some(WorkspaceObserved {
            ready_count,
            network_profile_matches,
        });
        Ok(Verification { checks })
    }

    async fn teardown(context: &RuntimeContext, prepared: &mut Self::Prepared) -> TeardownResult {
        let started = Instant::now();
        let baseline = prepared.session_baseline;
        let mut result =
            teardown_registered_sessions(context, prepared.sessions.clone(), baseline).await;
        result.checks.push(check_result(
            context,
            OperationId::CreateWorkspace,
            CheckId::WorkspaceRegistryBaseline,
            None,
            result.baseline_restored,
            format!("owned_session_count={baseline}"),
            format!(
                "destroyed={}/{},baseline_restored={}",
                result.destroyed_sessions,
                result.expected_destroyed_sessions,
                result.baseline_restored
            ),
            started,
        ));
        result
    }

    fn evidence(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(cell.workspace_count, outcomes.len())?;
        let created_count = u32::try_from(
            outcomes
                .iter()
                .filter(|outcome| outcome.output().is_some())
                .count(),
        )
        .unwrap_or(u32::MAX);
        let observed = prepared
            .observed
            .lock()
            .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
            .ok_or(ExecutorError::EvidenceUnavailable {
                operation: OperationId::CreateWorkspace,
                reason: "workspace verification did not complete",
            })?;
        Ok(OperationEvidence::CreateWorkspace(
            CreateWorkspaceEvidence {
                requested_count: cell.workspace_count,
                created_count,
                ready_count: observed.ready_count,
                destroyed_count: teardown.destroyed_sessions,
                network_profile_matches: observed.network_profile_matches,
                registry_baseline_restored: teardown.baseline_restored,
            },
        ))
    }
}

fn require_count(expected: u32, actual: usize) -> Result<(), ExecutorError> {
    if usize::try_from(expected).ok() != Some(actual) {
        return Err(ExecutorError::InvocationCount {
            operation: OperationId::CreateWorkspace,
            expected,
            actual,
        });
    }
    Ok(())
}
