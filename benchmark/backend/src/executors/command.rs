use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::daemon_session::{CreatedSession, WorkspaceSessionId, WorkspaceSessionLifecycle};
use crate::definitions::{
    CheckReference, ComparisonParticipation, ComparisonProjectionDefinition, FactorConstraint,
    FactorDefinition, FactorUnit, FactorValueKind, OperationDefinition, ProfileCatalogId,
    FACTOR_SCHEMA_REVISION, OPERATION_SEMANTIC_REVISION, SUPPORTED_COHORTS,
};
use crate::gateway::{ProductPath, MAX_COMMAND_TIMEOUT_MS};
use crate::model::{
    validate_factor, validate_nonzero_u32, CheckId, CleanupPolicy, CountSemantics, ExecutionShape,
    Factor, FactorId, FamilyId, IsolationPolicy, OperationEvidence, OperationId,
    OperationValidationError, ProductAccess, ProductOperation, ResolvedIsolationPolicy,
    SecurityClass, WorkspaceProfileId,
};

use super::{
    check_result, register_session, session_registry, teardown_registered_sessions, ExecutorError,
    InvocationOutcome, OperationLifecycle, ProductOutputStatus, ResponseMetadata, RuntimeContext,
    RuntimeInvocation, RuntimeOutput, SessionRegistry, TeardownResult, Verification,
};

const TEMPLATE_REVISION: u32 = 1;
pub(crate) const STORED_OUTPUT_LIMIT_BYTES: u64 = 65_536;

/// A verification probe is itself an `exec_command` request.  It must obey the
/// command operation's fixed gateway cap even when it verifies an operation
/// (such as LayerStack) with a longer trial timeout.
const fn probe_timeout_ms(trial_timeout_ms: u64) -> u64 {
    if trial_timeout_ms < MAX_COMMAND_TIMEOUT_MS {
        trial_timeout_ms
    } else {
        MAX_COMMAND_TIMEOUT_MS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSessionMode {
    Explicit,
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCase {
    Noop,
    Output64Kib,
    Cpu50Ms,
    FixtureRead,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandFactors {
    pub concurrent_requests: Factor<u32>,
    pub workspace_profile: Factor<WorkspaceProfileId>,
    pub session_mode: Factor<CommandSessionMode>,
    pub command_case: Factor<CommandCase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandPlan {
    pub enabled: bool,
    pub factors: ExecCommandFactors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandCell {
    pub concurrent_requests: u32,
    pub workspace_profile: WorkspaceProfileId,
    pub session_mode: CommandSessionMode,
    pub command_case: CommandCase,
    pub template_revision: u32,
    pub command: String,
    pub command_sha256: String,
    pub expected_exit_code: i32,
    pub output_limit_bytes: u64,
    pub resolved_isolation: ResolvedIsolationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundedOutputEvidence {
    pub byte_count: u64,
    pub truncated: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandEvidence {
    pub command_case: CommandCase,
    pub template_revision: u32,
    pub command_sha256: String,
    pub exit_code: Option<i32>,
    pub stdout: BoundedOutputEvidence,
    pub stderr: BoundedOutputEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandComparisonIdentity {
    pub concurrent_requests: u32,
    pub workspace_profile: WorkspaceProfileId,
    pub session_mode: CommandSessionMode,
    pub command_case: CommandCase,
    pub template_revision: u32,
    pub command_sha256: String,
}

const FACTORS: &[FactorDefinition] = &[
    FactorDefinition {
        id: FactorId::ConcurrentRequests,
        label: "Concurrent requests",
        help: "Independent exec_command requests released together from one trial barrier.",
        value_kind: FactorValueKind::UnsignedInteger,
        unit: Some(FactorUnit::Count),
        constraint: FactorConstraint::Positive,
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::WorkspaceProfile,
        label: "Workspace profile",
        help: "Deterministic materialized fixture scale available to the command.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::ProfileCatalog {
            catalog: ProfileCatalogId::WorkspaceProfiles,
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::SessionMode,
        label: "Session boundary",
        help: "Explicit measures against a prepared session; automatic includes create, publish, and destroy lifecycle.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["explicit", "automatic"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
    FactorDefinition {
        id: FactorId::CommandCase,
        label: "Command case",
        help: "Compile-time allowlisted bounded shell template; plans cannot submit arbitrary command text.",
        value_kind: FactorValueKind::Choice,
        unit: None,
        constraint: FactorConstraint::Choices {
            values: &["noop", "output64_kib", "cpu50_ms", "fixture_read"],
        },
        comparison: ComparisonParticipation::ScientificInvariant,
    },
];

const CHECKS: &[CheckReference] = &[
    CheckReference {
        id: CheckId::CommandExitStatus,
        label: "Command exit status",
        help: "The product result reports the case's exact expected exit status.",
        semantic_revision: 1,
        evidence_limit: 8,
    },
    CheckReference {
        id: CheckId::CommandOutput,
        label: "Bounded command output",
        help: "Stored stdout and stderr satisfy the case contract, cap, and digest checks.",
        semantic_revision: 1,
        evidence_limit: 8,
    },
    CheckReference {
        id: CheckId::CommandLifecycle,
        label: "Command lifecycle",
        help: "Session ownership and cleanup match the selected explicit or automatic boundary.",
        semantic_revision: 1,
        evidence_limit: 8,
    },
];

const COMPARISON_FACTORS: &[FactorId] = &[
    FactorId::ConcurrentRequests,
    FactorId::WorkspaceProfile,
    FactorId::SessionMode,
    FactorId::CommandCase,
];

pub const DEFINITION: OperationDefinition = OperationDefinition {
    id: OperationId::ExecCommand,
    family: FamilyId::Command,
    label: "Execute command",
    help: "Runs one of four compile-time allowlisted bounded shell cases through the public exec_command operation.",
    measured_boundary: "Explicit mode measures command admission and execution against a prepared session; automatic mode deliberately includes create, publish, and destroy lifecycle.",
    count_semantics_help: "Concurrent requests is the number of independent exec_command product requests released in one measured trial.",
    semantic_revision: OPERATION_SEMANTIC_REVISION,
    factor_schema_revision: FACTOR_SCHEMA_REVISION,
    count_semantics: CountSemantics::ConcurrentRequests {
        factor: FactorId::ConcurrentRequests,
    },
    execution_shape: ExecutionShape::BarrierRequestBatch,
    isolation: IsolationPolicy::SessionModeDependent,
    cleanup: CleanupPolicy::ResolveFromIsolation,
    product_access: ProductAccess::PublicGateway(ProductOperation::ExecCommand),
    supported_cohorts: SUPPORTED_COHORTS,
    security_class: SecurityClass::BoundedShell,
    factors: FACTORS,
    checks: CHECKS,
    phases: &[],
    comparison: ComparisonProjectionDefinition {
        semantic_revision: crate::definitions::COMPARISON_PROJECTION_REVISION,
        factors: COMPARISON_FACTORS,
    },
};

#[must_use]
pub fn validate(plan: &ExecCommandPlan) -> Vec<OperationValidationError> {
    let operation = OperationId::ExecCommand;
    let factors = &plan.factors;
    let mut errors = validate_factor(
        operation,
        FactorId::ConcurrentRequests,
        &factors.concurrent_requests,
    );
    errors.extend(validate_nonzero_u32(
        operation,
        FactorId::ConcurrentRequests,
        &factors.concurrent_requests,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::WorkspaceProfile,
        &factors.workspace_profile,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::SessionMode,
        &factors.session_mode,
    ));
    errors.extend(validate_factor(
        operation,
        FactorId::CommandCase,
        &factors.command_case,
    ));
    errors
}

pub fn expand(
    plan: &ExecCommandPlan,
) -> Result<Vec<ExecCommandCell>, Vec<OperationValidationError>> {
    let errors = validate(plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    if !plan.enabled {
        return Ok(Vec::new());
    }

    let factors = &plan.factors;
    let mut cells = Vec::new();
    for &concurrent_requests in &factors.concurrent_requests.values {
        for workspace_profile in &factors.workspace_profile.values {
            for &session_mode in &factors.session_mode.values {
                for &command_case in &factors.command_case.values {
                    let command = command_template(command_case);
                    cells.push(ExecCommandCell {
                        concurrent_requests,
                        workspace_profile: workspace_profile.clone(),
                        session_mode,
                        command_case,
                        template_revision: TEMPLATE_REVISION,
                        command_sha256: sha256(command.as_bytes()),
                        command: command.to_owned(),
                        expected_exit_code: 0,
                        output_limit_bytes: STORED_OUTPUT_LIMIT_BYTES,
                        resolved_isolation: match session_mode {
                            CommandSessionMode::Explicit => {
                                ResolvedIsolationPolicy::ReusableVerifiedFixture
                            }
                            CommandSessionMode::Automatic => {
                                ResolvedIsolationPolicy::FreshSandboxPerTrial
                            }
                        },
                    });
                }
            }
        }
    }
    Ok(cells)
}

#[must_use]
pub fn comparison_identity(cell: &ExecCommandCell) -> ExecCommandComparisonIdentity {
    ExecCommandComparisonIdentity {
        concurrent_requests: cell.concurrent_requests,
        workspace_profile: cell.workspace_profile.clone(),
        session_mode: cell.session_mode,
        command_case: cell.command_case,
        template_revision: cell.template_revision,
        command_sha256: cell.command_sha256.clone(),
    }
}

const fn command_template(case: CommandCase) -> &'static str {
    match case {
        CommandCase::Noop => "true",
        CommandCase::Output64Kib => "head -c 65536 /dev/zero | tr '\\000' x",
        CommandCase::Cpu50Ms => "i=0; while [ \"$i\" -lt 20000 ]; do i=$((i + 1)); done",
        CommandCase::FixtureRead => "wc -c < .eos-benchmark-fixture/command-read.bin",
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

const FIXTURE_READ_BYTES: usize = 65_536;

#[derive(Debug)]
pub struct ExecCommandRuntime;

#[derive(Debug)]
pub struct PreparedExecCommand {
    session: Option<CreatedSession>,
    sessions: SessionRegistry,
    session_baseline: usize,
    session_mode: CommandSessionMode,
}

#[derive(Debug, Clone)]
pub struct ExecCommandInvocation {
    request_id: String,
    session_id: Option<WorkspaceSessionId>,
    cell: ExecCommandCell,
}

impl RuntimeInvocation for ExecCommandInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Running,
    Ok,
    Error,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecCommandOutput {
    pub metadata: ResponseMetadata,
    pub status: ExecCommandStatus,
    pub exit_code: Option<i64>,
    pub wall_time_seconds: f64,
    pub command_total_time_seconds: f64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub total_lines: u64,
    pub original_token_count: u64,
    pub output: String,
    pub command_session_id: Option<String>,
    pub workspace_session_id: Option<String>,
    pub publish_rejected: bool,
    pub publish_reject_class: Option<String>,
}

impl RuntimeOutput for ExecCommandOutput {
    fn response_metadata(&self) -> &ResponseMetadata {
        &self.metadata
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecCommandWire {
    status: ExecCommandStatus,
    exit_code: Option<i64>,
    wall_time_seconds: f64,
    command_total_time_seconds: f64,
    start_offset: u64,
    end_offset: u64,
    total_lines: u64,
    original_token_count: u64,
    output: String,
    command_session_id: Option<String>,
    workspace_session_id: Option<String>,
    publish_rejected: Option<bool>,
    publish_reject_class: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupWriteWire {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    bytes_written: u64,
}

impl OperationLifecycle for ExecCommandRuntime {
    type Cell = ExecCommandCell;
    type Prepared = PreparedExecCommand;
    type Invocation = ExecCommandInvocation;
    type Output = ExecCommandOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        validate_runtime_cell(cell)?;
        let session_baseline = context.workspace_sessions().owned_session_count()?;
        if cell.command_case == CommandCase::FixtureRead {
            prepare_fixture_read(context).await?;
        }
        let sessions = session_registry();
        let session = match cell.session_mode {
            CommandSessionMode::Explicit => {
                let session = context
                    .workspace_sessions()
                    .create_no_op(
                        context.sandbox_id().clone(),
                        crate::model::AllowedNetworkProfile::Shared,
                        context.correlation("prepare-command-session")?,
                    )
                    .await?;
                register_session(&sessions, session.clone())?;
                Some(session)
            }
            CommandSessionMode::Automatic => None,
        };
        Ok(PreparedExecCommand {
            session,
            sessions,
            session_baseline,
            session_mode: cell.session_mode,
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
            .map(|index| ExecCommandInvocation {
                request_id: format!("command-{index}"),
                session_id: session_id.clone(),
                cell: cell.clone(),
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
                .exec_command(
                    context.sandbox_id(),
                    invocation.session_id.as_ref(),
                    &invocation.cell,
                    context.request_timeout_ms(),
                    context.request_timeout_ms(),
                    &context.correlation(&request_id)?,
                )
                .await?;
            parse_command_output(value)
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
            OperationId::ExecCommand,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let expected_session_id = prepared
            .session
            .as_ref()
            .map(|session| session.workspace_session_id().as_str());
        let mut checks = Vec::with_capacity(outcomes.len().saturating_mul(2));
        for outcome in outcomes {
            let started = Instant::now();
            let (exit_passed, exit_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => (
                    output.status == ExecCommandStatus::Ok
                        && output.exit_code == Some(i64::from(cell.expected_exit_code))
                        && !output.publish_rejected,
                    format!(
                        "status={:?},exit_code={:?},publish_rejected={}",
                        output.status, output.exit_code, output.publish_rejected
                    ),
                ),
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::ExecCommand,
                CheckId::CommandExitStatus,
                Some(outcome.request_id().to_owned()),
                exit_passed,
                format!("status=Ok,exit_code={}", cell.expected_exit_code),
                exit_actual,
                started,
            ));

            let started = Instant::now();
            let (output_passed, output_actual) = match outcome {
                InvocationOutcome::Succeeded { output, .. } => {
                    let expected = expected_output(cell.command_case);
                    let session_matches = match cell.session_mode {
                        CommandSessionMode::Explicit => {
                            output.workspace_session_id.as_deref() == expected_session_id
                        }
                        CommandSessionMode::Automatic => output.workspace_session_id.is_some(),
                    };
                    let cap = usize::try_from(cell.output_limit_bytes).unwrap_or(usize::MAX);
                    (
                        output.output == expected
                            && output.output.len() <= cap
                            && session_matches
                            && output.start_offset <= output.end_offset
                            && output.end_offset <= output.total_lines,
                        format!(
                            "bytes={},sha256={},session={:?},window={}-{}-{}",
                            output.output.len(),
                            sha256(output.output.as_bytes()),
                            output.workspace_session_id,
                            output.start_offset,
                            output.end_offset,
                            output.total_lines
                        ),
                    )
                }
                InvocationOutcome::Failed { error, .. } => (false, error.to_string()),
            };
            checks.push(check_result(
                context,
                OperationId::ExecCommand,
                CheckId::CommandOutput,
                Some(outcome.request_id().to_owned()),
                output_passed,
                format!(
                    "bytes={},sha256={}",
                    expected_output(cell.command_case).len(),
                    sha256(expected_output(cell.command_case).as_bytes())
                ),
                output_actual,
                started,
            ));
        }
        Ok(Verification { checks })
    }

    async fn teardown(context: &RuntimeContext, prepared: &mut Self::Prepared) -> TeardownResult {
        let started = Instant::now();
        let mode = prepared.session_mode;
        let baseline = prepared.session_baseline;
        let mut result =
            teardown_registered_sessions(context, prepared.sessions.clone(), baseline).await;
        result.checks.push(check_result(
            context,
            OperationId::ExecCommand,
            CheckId::CommandLifecycle,
            None,
            result.baseline_restored,
            format!("mode={mode:?},registry_count={baseline}"),
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
        _prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        _teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError> {
        require_count(
            OperationId::ExecCommand,
            cell.concurrent_requests,
            outcomes.len(),
        )?;
        let output = outcomes.iter().find_map(InvocationOutcome::output).ok_or(
            ExecutorError::EvidenceUnavailable {
                operation: OperationId::ExecCommand,
                reason: "no successful command response",
            },
        )?;
        let truncated =
            output.output.len() > usize::try_from(cell.output_limit_bytes).unwrap_or(usize::MAX);
        Ok(OperationEvidence::ExecCommand(ExecCommandEvidence {
            command_case: cell.command_case,
            template_revision: cell.template_revision,
            command_sha256: cell.command_sha256.clone(),
            exit_code: output.exit_code.and_then(|value| i32::try_from(value).ok()),
            stdout: BoundedOutputEvidence {
                byte_count: u64::try_from(output.output.len()).unwrap_or(u64::MAX),
                truncated,
                sha256: sha256(output.output.as_bytes()),
            },
            // The fixed templates never write stderr. The product deliberately
            // exposes one bounded transcript, so successful exact-output checks
            // prove the stderr projection is empty for these allowlisted cases.
            stderr: BoundedOutputEvidence {
                byte_count: 0,
                truncated: false,
                sha256: sha256(&[]),
            },
        }))
    }
}

fn validate_runtime_cell(cell: &ExecCommandCell) -> Result<(), ExecutorError> {
    let expected_command = command_template(cell.command_case);
    if cell.concurrent_requests == 0
        || cell.template_revision != TEMPLATE_REVISION
        || cell.command != expected_command
        || cell.command_sha256 != sha256(expected_command.as_bytes())
        || cell.expected_exit_code != 0
        || cell.output_limit_bytes != STORED_OUTPUT_LIMIT_BYTES
    {
        return Err(ExecutorError::InvalidFixture {
            operation: OperationId::ExecCommand,
            reason: "expanded command cell does not match the compile-time allowlist",
        });
    }
    Ok(())
}

async fn prepare_fixture_read(context: &RuntimeContext) -> Result<(), ExecutorError> {
    let path = ProductPath::new(".eos-benchmark-fixture/command-read.bin")?;
    let content = "f".repeat(FIXTURE_READ_BYTES);
    let value = context
        .gateway()
        .file_write(
            context.sandbox_id(),
            None,
            &path,
            &content,
            &context.correlation("prepare-command-fixture")?,
        )
        .await?;
    let wire: SetupWriteWire =
        serde_json::from_value(value).map_err(|_| ExecutorError::ResponseSchema {
            operation: OperationId::ExecCommand,
            detail: "fixture write",
        })?;
    if !matches!(wire.kind.as_str(), "create" | "update")
        || wire.path != path.as_str()
        || wire.bytes_written != FIXTURE_READ_BYTES as u64
    {
        return Err(ExecutorError::ResponseSchema {
            operation: OperationId::ExecCommand,
            detail: "fixture write values",
        });
    }
    Ok(())
}

fn parse_command_output(value: Value) -> Result<ExecCommandOutput, ExecutorError> {
    let wire: ExecCommandWire =
        serde_json::from_value(value.clone()).map_err(|_| ExecutorError::ResponseSchema {
            operation: OperationId::ExecCommand,
            detail: "command output",
        })?;
    let publish_rejected = wire.publish_rejected.unwrap_or(false);
    if !wire.wall_time_seconds.is_finite()
        || wire.wall_time_seconds < 0.0
        || !wire.command_total_time_seconds.is_finite()
        || wire.command_total_time_seconds < 0.0
        || (publish_rejected != wire.publish_reject_class.is_some())
        || wire.publish_rejected == Some(false)
    {
        return Err(ExecutorError::ResponseSchema {
            operation: OperationId::ExecCommand,
            detail: "command output values",
        });
    }
    let product_status = match wire.status {
        ExecCommandStatus::Running => ProductOutputStatus::Running,
        ExecCommandStatus::Ok => ProductOutputStatus::Succeeded,
        ExecCommandStatus::Error => ProductOutputStatus::Failed,
        ExecCommandStatus::TimedOut => ProductOutputStatus::TimedOut,
        ExecCommandStatus::Cancelled => ProductOutputStatus::Cancelled,
    };
    Ok(ExecCommandOutput {
        metadata: super::response_metadata(&value, product_status)?,
        status: wire.status,
        exit_code: wire.exit_code,
        wall_time_seconds: wire.wall_time_seconds,
        command_total_time_seconds: wire.command_total_time_seconds,
        start_offset: wire.start_offset,
        end_offset: wire.end_offset,
        total_lines: wire.total_lines,
        original_token_count: wire.original_token_count,
        output: wire.output,
        command_session_id: wire.command_session_id,
        workspace_session_id: wire.workspace_session_id,
        publish_rejected,
        publish_reject_class: wire.publish_reject_class,
    })
}

fn expected_output(case: CommandCase) -> String {
    match case {
        CommandCase::Noop | CommandCase::Cpu50Ms => String::new(),
        CommandCase::Output64Kib => "x".repeat(FIXTURE_READ_BYTES),
        CommandCase::FixtureRead => FIXTURE_READ_BYTES.to_string(),
    }
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

/// Bounded readiness/usability probe used by the workspace and LayerStack
/// verifiers. It can only issue the same compile-time no-op command cell.
pub(crate) async fn probe_session(
    context: &RuntimeContext,
    session_id: &WorkspaceSessionId,
    request_id: &str,
) -> Result<(), ExecutorError> {
    let timeout_ms = probe_timeout_ms(context.request_timeout_ms());
    let command = command_template(CommandCase::Noop).to_owned();
    let cell = ExecCommandCell {
        concurrent_requests: 1,
        workspace_profile: WorkspaceProfileId::new("small").map_err(|_| {
            ExecutorError::InvalidFixture {
                operation: OperationId::ExecCommand,
                reason: "fixed probe profile",
            }
        })?,
        session_mode: CommandSessionMode::Explicit,
        command_case: CommandCase::Noop,
        template_revision: TEMPLATE_REVISION,
        command_sha256: sha256(command.as_bytes()),
        command,
        expected_exit_code: 0,
        output_limit_bytes: STORED_OUTPUT_LIMIT_BYTES,
        resolved_isolation: ResolvedIsolationPolicy::ReusableVerifiedFixture,
    };
    let value = context
        .gateway()
        .exec_command(
            context.sandbox_id(),
            Some(session_id),
            &cell,
            timeout_ms,
            timeout_ms,
            &context.correlation(request_id)?,
        )
        .await?;
    let output = parse_command_output(value)?;
    if output.status != ExecCommandStatus::Ok
        || output.exit_code != Some(0)
        || !output.output.is_empty()
        || output.publish_rejected
        || output.workspace_session_id.as_deref() != Some(session_id.as_str())
    {
        return Err(ExecutorError::ResponseSchema {
            operation: OperationId::ExecCommand,
            detail: "session usability probe",
        });
    }
    Ok(())
}

#[cfg(test)]
mod runtime_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn command_wire_is_strict_and_validates_publish_pair() {
        let value = json!({
            "status": "ok",
            "exit_code": 0,
            "wall_time_seconds": 0.01,
            "command_total_time_seconds": 0.01,
            "start_offset": 0,
            "end_offset": 0,
            "total_lines": 0,
            "original_token_count": 0,
            "output": "",
            "workspace_session_id": "session-1"
        });
        assert!(parse_command_output(value.clone()).is_ok());
        let mut unknown = value.clone();
        unknown["extra"] = json!(true);
        assert!(parse_command_output(unknown).is_err());
        let mut unpaired = value;
        unpaired["publish_rejected"] = json!(true);
        assert!(parse_command_output(unpaired).is_err());
    }

    #[test]
    fn runtime_cell_rechecks_compile_time_allowlist() {
        let command = command_template(CommandCase::Noop).to_owned();
        let mut cell = ExecCommandCell {
            concurrent_requests: 1,
            workspace_profile: WorkspaceProfileId::new("small").expect("profile"),
            session_mode: CommandSessionMode::Explicit,
            command_case: CommandCase::Noop,
            template_revision: TEMPLATE_REVISION,
            command_sha256: sha256(command.as_bytes()),
            command,
            expected_exit_code: 0,
            output_limit_bytes: STORED_OUTPUT_LIMIT_BYTES,
            resolved_isolation: ResolvedIsolationPolicy::ReusableVerifiedFixture,
        };
        assert!(validate_runtime_cell(&cell).is_ok());
        cell.command = "uname -a".to_owned();
        assert!(validate_runtime_cell(&cell).is_err());
    }

    #[test]
    fn usability_probe_uses_the_command_cap_not_a_parent_trial_cap() {
        assert_eq!(probe_timeout_ms(30_000), 30_000);
        assert_eq!(
            probe_timeout_ms(MAX_COMMAND_TIMEOUT_MS),
            MAX_COMMAND_TIMEOUT_MS
        );
        assert_eq!(probe_timeout_ms(600_000), MAX_COMMAND_TIMEOUT_MS);
    }
}
