pub mod command;
pub mod files;
pub mod layerstack;
pub mod workspace;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::checks::{
    bounded_evidence, check_definition, CheckEvidenceItem, CheckResult, CheckVerdict,
};
use crate::daemon_session::{
    CreatedSession, WorkspaceSessionAdapter, WorkspaceSessionError, WorkspaceSessionLifecycle,
};
use crate::gateway::{Correlation, GatewayError, OwnedSandboxId, ProductGateway};
use crate::model::{CheckId, OperationEvidence, OperationId};
use crate::model::{ExpandedOperationCell, OperationPlan, OperationValidationError};

/// Concrete, closed runtime dependencies shared by the seven operation
/// implementations. The scheduler constructs one context for a prepared trial;
/// operation code cannot replace either product access surface.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    gateway: Arc<ProductGateway>,
    workspace_sessions: Arc<WorkspaceSessionAdapter>,
    sandbox_id: OwnedSandboxId,
    run_id: String,
    cell_id: String,
    trial_id: String,
    request_timeout_ms: u64,
    gateway_remount_parallelism: u32,
}

impl RuntimeContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gateway: Arc<ProductGateway>,
        workspace_sessions: Arc<WorkspaceSessionAdapter>,
        sandbox_id: OwnedSandboxId,
        run_id: impl Into<String>,
        cell_id: impl Into<String>,
        trial_id: impl Into<String>,
        request_timeout_ms: u64,
        gateway_remount_parallelism: u32,
    ) -> Result<Self, ExecutorError> {
        if request_timeout_ms == 0 {
            return Err(ExecutorError::InvalidRuntime(
                "request timeout must be positive",
            ));
        }
        if gateway_remount_parallelism == 0 {
            return Err(ExecutorError::InvalidRuntime(
                "gateway remount parallelism must be positive",
            ));
        }
        let run_id = run_id.into();
        let cell_id = cell_id.into();
        let trial_id = trial_id.into();
        Correlation::new(&run_id, &cell_id, &trial_id, "context-validation")?;
        Ok(Self {
            gateway,
            workspace_sessions,
            sandbox_id,
            run_id,
            cell_id,
            trial_id,
            request_timeout_ms,
            gateway_remount_parallelism,
        })
    }

    #[must_use]
    pub fn gateway(&self) -> &ProductGateway {
        &self.gateway
    }

    #[must_use]
    pub fn workspace_sessions(&self) -> &WorkspaceSessionAdapter {
        &self.workspace_sessions
    }

    #[must_use]
    pub fn sandbox_id(&self) -> &OwnedSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn cell_id(&self) -> &str {
        &self.cell_id
    }

    #[must_use]
    pub fn trial_id(&self) -> &str {
        &self.trial_id
    }

    #[must_use]
    pub const fn request_timeout_ms(&self) -> u64 {
        self.request_timeout_ms
    }

    #[must_use]
    pub const fn gateway_remount_parallelism(&self) -> u32 {
        self.gateway_remount_parallelism
    }

    pub fn correlation(&self, request_id: impl Into<String>) -> Result<Correlation, ExecutorError> {
        Ok(Correlation::new(
            &self.run_id,
            &self.cell_id,
            &self.trial_id,
            request_id,
        )?)
    }

    /// Filesystem-safe deterministic fixture key. User-authored identifiers are
    /// never copied into product paths.
    #[must_use]
    pub fn fixture_key(&self) -> String {
        let mut digest = Sha256::new();
        for value in [&self.run_id, &self.cell_id, &self.trial_id] {
            digest.update(value.len().to_le_bytes());
            digest.update(value.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    WorkspaceSession(#[from] WorkspaceSessionError),
    #[error("invalid executor runtime: {0}")]
    InvalidRuntime(&'static str),
    #[error("{operation:?} product response schema mismatch: {detail}")]
    ResponseSchema {
        operation: OperationId,
        detail: &'static str,
    },
    #[error("{operation:?} fixture is invalid: {reason}")]
    InvalidFixture {
        operation: OperationId,
        reason: &'static str,
    },
    #[error("{operation:?} invocation count mismatch: expected {expected}, received {actual}")]
    InvocationCount {
        operation: OperationId,
        expected: u32,
        actual: usize,
    },
    #[error("{operation:?} product request exceeded its {timeout_ms} ms deadline")]
    RequestTimedOut {
        operation: OperationId,
        timeout_ms: u64,
    },
    #[error("{operation:?} product request was cancelled")]
    RequestCancelled { operation: OperationId },
    #[error("executor dispatch expected {expected:?}, received {actual:?}")]
    DispatchMismatch {
        expected: OperationId,
        actual: OperationId,
    },
    #[error("{operation:?} invocation failed before verification: {detail}")]
    DispatchedInvocationFailure {
        operation: OperationId,
        detail: String,
    },
    #[error("{operation:?} requires unavailable product contract: {capability}")]
    UnsupportedProductContract {
        operation: OperationId,
        capability: &'static str,
    },
    #[error("executor session registry is unavailable")]
    SessionRegistryUnavailable,
    #[error("{operation:?} evidence is unavailable: {reason}")]
    EvidenceUnavailable {
        operation: OperationId,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductOutputStatus {
    Succeeded,
    Running,
    Failed,
    TimedOut,
    Cancelled,
}

impl ProductOutputStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseMetadata {
    pub status: ProductOutputStatus,
    pub response_bytes: u64,
    pub bounded_response_sha256: String,
}

pub trait RuntimeOutput {
    fn response_metadata(&self) -> &ResponseMetadata;
}

#[derive(Debug)]
pub enum InvocationOutcome<T> {
    Succeeded {
        request_id: String,
        output: T,
    },
    Failed {
        request_id: String,
        error: ExecutorError,
    },
}

impl<T> InvocationOutcome<T> {
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::Succeeded { request_id, .. } | Self::Failed { request_id, .. } => request_id,
        }
    }

    #[must_use]
    pub fn output(&self) -> Option<&T> {
        match self {
            Self::Succeeded { output, .. } => Some(output),
            Self::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<&ExecutorError> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { error, .. } => Some(error),
        }
    }

    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }
}

pub trait RuntimeInvocation {
    fn request_id(&self) -> &str;
}

#[derive(Debug, Default)]
pub struct Verification {
    pub checks: Vec<CheckResult>,
}

#[derive(Debug)]
pub struct TeardownResult {
    pub checks: Vec<CheckResult>,
    pub expected_destroyed_sessions: u32,
    pub destroyed_sessions: u32,
    pub baseline_restored: bool,
    pub errors: Vec<ExecutorError>,
}

impl TeardownResult {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            checks: Vec::new(),
            expected_destroyed_sessions: 0,
            destroyed_sessions: 0,
            baseline_restored: true,
            errors: Vec::new(),
        }
    }
}

/// Static lifecycle contract. Every operation supplies its own typed cell,
/// prepared state, invocation, and output; no erased workflow/plugin surface is
/// available.
#[allow(async_fn_in_trait)]
pub trait OperationLifecycle {
    type Cell;
    type Prepared;
    type Invocation: RuntimeInvocation;
    type Output: RuntimeOutput;

    async fn prepare(
        context: &RuntimeContext,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError>;

    fn invocations(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError>;

    async fn invoke_one(
        context: &RuntimeContext,
        invocation: Self::Invocation,
    ) -> InvocationOutcome<Self::Output>;

    async fn verify(
        context: &RuntimeContext,
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
    ) -> Result<Verification, ExecutorError>;

    async fn teardown(context: &RuntimeContext, prepared: &mut Self::Prepared) -> TeardownResult;

    fn evidence(
        prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[InvocationOutcome<Self::Output>],
        teardown: &TeardownResult,
    ) -> Result<OperationEvidence, ExecutorError>;
}

pub(crate) type SessionRegistry = Arc<Mutex<Vec<CreatedSession>>>;

pub(crate) fn session_registry() -> SessionRegistry {
    Arc::new(Mutex::new(Vec::new()))
}

pub(crate) fn register_session(
    sessions: &SessionRegistry,
    session: CreatedSession,
) -> Result<(), ExecutorError> {
    sessions
        .lock()
        .map_err(|_| ExecutorError::SessionRegistryUnavailable)?
        .push(session);
    Ok(())
}

pub(crate) fn registered_sessions(
    sessions: &SessionRegistry,
) -> Result<Vec<CreatedSession>, ExecutorError> {
    sessions
        .lock()
        .map(|sessions| sessions.clone())
        .map_err(|_| ExecutorError::SessionRegistryUnavailable)
}

pub(crate) async fn teardown_registered_sessions(
    context: &RuntimeContext,
    sessions: SessionRegistry,
    baseline: usize,
) -> TeardownResult {
    let owned = match sessions.lock() {
        Ok(mut sessions) => std::mem::take(&mut *sessions),
        Err(_) => {
            return TeardownResult {
                checks: Vec::new(),
                expected_destroyed_sessions: 0,
                destroyed_sessions: 0,
                baseline_restored: false,
                errors: vec![ExecutorError::SessionRegistryUnavailable],
            };
        }
    };
    let expected_destroyed_sessions = u32::try_from(owned.len()).unwrap_or(u32::MAX);
    let mut destroyed_sessions = 0_u32;
    let mut errors = Vec::new();
    for (index, session) in owned.into_iter().enumerate() {
        let correlation = match context.correlation(format!("teardown-session-{index}")) {
            Ok(correlation) => correlation,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let (sandbox_id, session_id) = session.into_parts();
        match context
            .workspace_sessions()
            .destroy(sandbox_id, session_id, correlation)
            .await
        {
            Ok(()) => destroyed_sessions = destroyed_sessions.saturating_add(1),
            Err(error) => errors.push(error.into()),
        }
    }
    let count_matches = context
        .workspace_sessions()
        .owned_session_count()
        .is_ok_and(|count| count == baseline);
    TeardownResult {
        checks: Vec::new(),
        expected_destroyed_sessions,
        destroyed_sessions,
        baseline_restored: errors.is_empty() && count_matches,
        errors,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_result(
    context: &RuntimeContext,
    operation: OperationId,
    id: CheckId,
    request_id: Option<String>,
    passed: bool,
    expected: impl Into<String>,
    actual: impl Into<String>,
    started: Instant,
) -> CheckResult {
    let definition = check_definition(id);
    let duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    CheckResult {
        id,
        semantic_revision: definition.semantic_revision,
        operation_id: operation,
        cell_id: context.cell_id().to_owned(),
        trial_id: context.trial_id().to_owned(),
        request_id,
        verdict: if passed {
            CheckVerdict::Pass
        } else {
            CheckVerdict::Fail
        },
        duration_ns,
        evidence: bounded_evidence(
            id,
            vec![CheckEvidenceItem {
                expected: expected.into(),
                actual: actual.into(),
                artifact_id: None,
            }],
        ),
    }
}

pub(crate) fn response_metadata(
    value: &Value,
    status: ProductOutputStatus,
) -> Result<ResponseMetadata, ExecutorError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        ExecutorError::InvalidRuntime("verified product response could not be canonically encoded")
    })?;
    Ok(ResponseMetadata {
        status,
        response_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        bounded_response_sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
    })
}

/// Closed prepared-state sum used by the scheduler. Adding an operation to the
/// model requires updating every exhaustive dispatch below.
#[derive(Debug)]
pub enum PreparedOperation {
    ExecCommand(command::PreparedExecCommand),
    FileRead(files::PreparedFileRead),
    FileWrite(files::PreparedFileWrite),
    FileEdit(files::PreparedFileEdit),
    FileBlame(files::PreparedFileBlame),
    CreateWorkspace(workspace::PreparedCreateWorkspace),
    SquashLayerstack(layerstack::PreparedSquashLayerstack),
}

impl PreparedOperation {
    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
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

    pub fn squash_layerstack_partial_evidence(
        &self,
    ) -> Result<layerstack::SquashLayerstackPartialEvidence, ExecutorError> {
        match self {
            Self::SquashLayerstack(prepared) => prepared.partial_evidence(),
            Self::ExecCommand(_)
            | Self::FileRead(_)
            | Self::FileWrite(_)
            | Self::FileEdit(_)
            | Self::FileBlame(_)
            | Self::CreateWorkspace(_) => Err(ExecutorError::DispatchMismatch {
                expected: OperationId::SquashLayerstack,
                actual: self.operation_id(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum OperationInvocation {
    ExecCommand(command::ExecCommandInvocation),
    FileRead(files::FileReadInvocation),
    FileWrite(files::FileWriteInvocation),
    FileEdit(files::FileEditInvocation),
    FileBlame(files::FileBlameInvocation),
    CreateWorkspace(workspace::CreateWorkspaceInvocation),
    SquashLayerstack(layerstack::SquashLayerstackInvocation),
}

impl OperationInvocation {
    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
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

impl RuntimeInvocation for OperationInvocation {
    fn request_id(&self) -> &str {
        match self {
            Self::ExecCommand(invocation) => invocation.request_id(),
            Self::FileRead(invocation) => invocation.request_id(),
            Self::FileWrite(invocation) => invocation.request_id(),
            Self::FileEdit(invocation) => invocation.request_id(),
            Self::FileBlame(invocation) => invocation.request_id(),
            Self::CreateWorkspace(invocation) => invocation.request_id(),
            Self::SquashLayerstack(invocation) => invocation.request_id(),
        }
    }
}

#[derive(Debug)]
pub enum OperationOutcome {
    ExecCommand(InvocationOutcome<command::ExecCommandOutput>),
    FileRead(InvocationOutcome<files::FileReadOutput>),
    FileWrite(InvocationOutcome<files::FileWriteOutput>),
    FileEdit(InvocationOutcome<files::FileEditOutput>),
    FileBlame(InvocationOutcome<files::FileBlameOutput>),
    CreateWorkspace(InvocationOutcome<workspace::CreateWorkspaceOutput>),
    SquashLayerstack(InvocationOutcome<layerstack::SquashLayerstackOutput>),
}

impl OperationOutcome {
    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
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
    pub fn request_id(&self) -> &str {
        match self {
            Self::ExecCommand(outcome) => outcome.request_id(),
            Self::FileRead(outcome) => outcome.request_id(),
            Self::FileWrite(outcome) => outcome.request_id(),
            Self::FileEdit(outcome) => outcome.request_id(),
            Self::FileBlame(outcome) => outcome.request_id(),
            Self::CreateWorkspace(outcome) => outcome.request_id(),
            Self::SquashLayerstack(outcome) => outcome.request_id(),
        }
    }

    #[must_use]
    pub fn response_metadata(&self) -> Option<&ResponseMetadata> {
        match self {
            Self::ExecCommand(outcome) => outcome.output().map(RuntimeOutput::response_metadata),
            Self::FileRead(outcome) => outcome.output().map(RuntimeOutput::response_metadata),
            Self::FileWrite(outcome) => outcome.output().map(RuntimeOutput::response_metadata),
            Self::FileEdit(outcome) => outcome.output().map(RuntimeOutput::response_metadata),
            Self::FileBlame(outcome) => outcome.output().map(RuntimeOutput::response_metadata),
            Self::CreateWorkspace(outcome) => {
                outcome.output().map(RuntimeOutput::response_metadata)
            }
            Self::SquashLayerstack(outcome) => {
                outcome.output().map(RuntimeOutput::response_metadata)
            }
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<&ExecutorError> {
        match self {
            Self::ExecCommand(outcome) => outcome.error(),
            Self::FileRead(outcome) => outcome.error(),
            Self::FileWrite(outcome) => outcome.error(),
            Self::FileEdit(outcome) => outcome.error(),
            Self::FileBlame(outcome) => outcome.error(),
            Self::CreateWorkspace(outcome) => outcome.error(),
            Self::SquashLayerstack(outcome) => outcome.error(),
        }
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        self.error().is_none()
    }

    #[must_use]
    pub fn failed(
        operation: OperationId,
        request_id: impl Into<String>,
        error: ExecutorError,
    ) -> Self {
        let request_id = request_id.into();
        match operation {
            OperationId::ExecCommand => {
                Self::ExecCommand(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::FileRead => {
                Self::FileRead(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::FileWrite => {
                Self::FileWrite(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::FileEdit => {
                Self::FileEdit(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::FileBlame => {
                Self::FileBlame(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::CreateWorkspace => {
                Self::CreateWorkspace(InvocationOutcome::Failed { request_id, error })
            }
            OperationId::SquashLayerstack => {
                Self::SquashLayerstack(InvocationOutcome::Failed { request_id, error })
            }
        }
    }
}

fn dispatch_mismatch(expected: OperationId, actual: OperationId) -> ExecutorError {
    ExecutorError::DispatchMismatch { expected, actual }
}

fn verification_outcome<T: Clone>(
    outcome: &InvocationOutcome<T>,
    operation: OperationId,
) -> InvocationOutcome<T> {
    match outcome {
        InvocationOutcome::Succeeded { request_id, output } => InvocationOutcome::Succeeded {
            request_id: request_id.clone(),
            output: output.clone(),
        },
        InvocationOutcome::Failed { request_id, error } => InvocationOutcome::Failed {
            request_id: request_id.clone(),
            error: ExecutorError::DispatchedInvocationFailure {
                operation,
                detail: error.to_string(),
            },
        },
    }
}

macro_rules! collect_dispatch_outcomes {
    ($outcomes:expr, $variant:ident, $operation:expr) => {{
        $outcomes
            .iter()
            .map(|outcome| match outcome {
                OperationOutcome::$variant(outcome) => {
                    Ok(verification_outcome(outcome, $operation))
                }
                other => Err(dispatch_mismatch($operation, other.operation_id())),
            })
            .collect::<Result<Vec<_>, ExecutorError>>()?
    }};
}

pub async fn prepare_operation(
    context: &RuntimeContext,
    cell: &ExpandedOperationCell,
) -> Result<PreparedOperation, ExecutorError> {
    match cell {
        ExpandedOperationCell::ExecCommand(cell) => {
            command::ExecCommandRuntime::prepare(context, cell)
                .await
                .map(PreparedOperation::ExecCommand)
        }
        ExpandedOperationCell::FileRead(cell) => files::FileReadRuntime::prepare(context, cell)
            .await
            .map(PreparedOperation::FileRead),
        ExpandedOperationCell::FileWrite(cell) => files::FileWriteRuntime::prepare(context, cell)
            .await
            .map(PreparedOperation::FileWrite),
        ExpandedOperationCell::FileEdit(cell) => files::FileEditRuntime::prepare(context, cell)
            .await
            .map(PreparedOperation::FileEdit),
        ExpandedOperationCell::FileBlame(cell) => files::FileBlameRuntime::prepare(context, cell)
            .await
            .map(PreparedOperation::FileBlame),
        ExpandedOperationCell::CreateWorkspace(cell) => {
            workspace::CreateWorkspaceRuntime::prepare(context, cell)
                .await
                .map(PreparedOperation::CreateWorkspace)
        }
        ExpandedOperationCell::SquashLayerstack(cell) => {
            layerstack::SquashLayerstackRuntime::prepare(context, cell)
                .await
                .map(PreparedOperation::SquashLayerstack)
        }
    }
}

pub fn operation_invocations(
    prepared: &PreparedOperation,
    cell: &ExpandedOperationCell,
) -> Result<Vec<OperationInvocation>, ExecutorError> {
    match prepared {
        PreparedOperation::ExecCommand(prepared) => {
            let ExpandedOperationCell::ExecCommand(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::ExecCommand, cell.id()));
            };
            command::ExecCommandRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::ExecCommand)
                    .collect()
            })
        }
        PreparedOperation::FileRead(prepared) => {
            let ExpandedOperationCell::FileRead(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileRead, cell.id()));
            };
            files::FileReadRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::FileRead)
                    .collect()
            })
        }
        PreparedOperation::FileWrite(prepared) => {
            let ExpandedOperationCell::FileWrite(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileWrite, cell.id()));
            };
            files::FileWriteRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::FileWrite)
                    .collect()
            })
        }
        PreparedOperation::FileEdit(prepared) => {
            let ExpandedOperationCell::FileEdit(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileEdit, cell.id()));
            };
            files::FileEditRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::FileEdit)
                    .collect()
            })
        }
        PreparedOperation::FileBlame(prepared) => {
            let ExpandedOperationCell::FileBlame(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileBlame, cell.id()));
            };
            files::FileBlameRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::FileBlame)
                    .collect()
            })
        }
        PreparedOperation::CreateWorkspace(prepared) => {
            let ExpandedOperationCell::CreateWorkspace(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::CreateWorkspace, cell.id()));
            };
            workspace::CreateWorkspaceRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::CreateWorkspace)
                    .collect()
            })
        }
        PreparedOperation::SquashLayerstack(prepared) => {
            let ExpandedOperationCell::SquashLayerstack(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::SquashLayerstack, cell.id()));
            };
            layerstack::SquashLayerstackRuntime::invocations(prepared, cell).map(|invocations| {
                invocations
                    .into_iter()
                    .map(OperationInvocation::SquashLayerstack)
                    .collect()
            })
        }
    }
}

pub async fn invoke_operation(
    context: &RuntimeContext,
    invocation: OperationInvocation,
) -> OperationOutcome {
    match invocation {
        OperationInvocation::ExecCommand(invocation) => OperationOutcome::ExecCommand(
            command::ExecCommandRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::FileRead(invocation) => OperationOutcome::FileRead(
            files::FileReadRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::FileWrite(invocation) => OperationOutcome::FileWrite(
            files::FileWriteRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::FileEdit(invocation) => OperationOutcome::FileEdit(
            files::FileEditRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::FileBlame(invocation) => OperationOutcome::FileBlame(
            files::FileBlameRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::CreateWorkspace(invocation) => OperationOutcome::CreateWorkspace(
            workspace::CreateWorkspaceRuntime::invoke_one(context, invocation).await,
        ),
        OperationInvocation::SquashLayerstack(invocation) => OperationOutcome::SquashLayerstack(
            layerstack::SquashLayerstackRuntime::invoke_one(context, invocation).await,
        ),
    }
}

pub async fn verify_operation(
    context: &RuntimeContext,
    prepared: &PreparedOperation,
    cell: &ExpandedOperationCell,
    outcomes: &[OperationOutcome],
) -> Result<Verification, ExecutorError> {
    match prepared {
        PreparedOperation::ExecCommand(prepared) => {
            let ExpandedOperationCell::ExecCommand(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::ExecCommand, cell.id()));
            };
            let outcomes =
                collect_dispatch_outcomes!(outcomes, ExecCommand, OperationId::ExecCommand);
            command::ExecCommandRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::FileRead(prepared) => {
            let ExpandedOperationCell::FileRead(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileRead, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileRead, OperationId::FileRead);
            files::FileReadRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::FileWrite(prepared) => {
            let ExpandedOperationCell::FileWrite(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileWrite, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileWrite, OperationId::FileWrite);
            files::FileWriteRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::FileEdit(prepared) => {
            let ExpandedOperationCell::FileEdit(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileEdit, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileEdit, OperationId::FileEdit);
            files::FileEditRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::FileBlame(prepared) => {
            let ExpandedOperationCell::FileBlame(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileBlame, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileBlame, OperationId::FileBlame);
            files::FileBlameRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::CreateWorkspace(prepared) => {
            let ExpandedOperationCell::CreateWorkspace(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::CreateWorkspace, cell.id()));
            };
            let outcomes =
                collect_dispatch_outcomes!(outcomes, CreateWorkspace, OperationId::CreateWorkspace);
            workspace::CreateWorkspaceRuntime::verify(context, prepared, cell, &outcomes).await
        }
        PreparedOperation::SquashLayerstack(prepared) => {
            let ExpandedOperationCell::SquashLayerstack(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::SquashLayerstack, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(
                outcomes,
                SquashLayerstack,
                OperationId::SquashLayerstack
            );
            layerstack::SquashLayerstackRuntime::verify(context, prepared, cell, &outcomes).await
        }
    }
}

pub async fn teardown_operation(
    context: &RuntimeContext,
    prepared: &mut PreparedOperation,
) -> TeardownResult {
    match prepared {
        PreparedOperation::ExecCommand(prepared) => {
            command::ExecCommandRuntime::teardown(context, prepared).await
        }
        PreparedOperation::FileRead(prepared) => {
            files::FileReadRuntime::teardown(context, prepared).await
        }
        PreparedOperation::FileWrite(prepared) => {
            files::FileWriteRuntime::teardown(context, prepared).await
        }
        PreparedOperation::FileEdit(prepared) => {
            files::FileEditRuntime::teardown(context, prepared).await
        }
        PreparedOperation::FileBlame(prepared) => {
            files::FileBlameRuntime::teardown(context, prepared).await
        }
        PreparedOperation::CreateWorkspace(prepared) => {
            workspace::CreateWorkspaceRuntime::teardown(context, prepared).await
        }
        PreparedOperation::SquashLayerstack(prepared) => {
            layerstack::SquashLayerstackRuntime::teardown(context, prepared).await
        }
    }
}

pub fn operation_evidence(
    prepared: &PreparedOperation,
    cell: &ExpandedOperationCell,
    outcomes: &[OperationOutcome],
    teardown: &TeardownResult,
) -> Result<OperationEvidence, ExecutorError> {
    match prepared {
        PreparedOperation::ExecCommand(prepared) => {
            let ExpandedOperationCell::ExecCommand(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::ExecCommand, cell.id()));
            };
            let outcomes =
                collect_dispatch_outcomes!(outcomes, ExecCommand, OperationId::ExecCommand);
            command::ExecCommandRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::FileRead(prepared) => {
            let ExpandedOperationCell::FileRead(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileRead, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileRead, OperationId::FileRead);
            files::FileReadRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::FileWrite(prepared) => {
            let ExpandedOperationCell::FileWrite(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileWrite, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileWrite, OperationId::FileWrite);
            files::FileWriteRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::FileEdit(prepared) => {
            let ExpandedOperationCell::FileEdit(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileEdit, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileEdit, OperationId::FileEdit);
            files::FileEditRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::FileBlame(prepared) => {
            let ExpandedOperationCell::FileBlame(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::FileBlame, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(outcomes, FileBlame, OperationId::FileBlame);
            files::FileBlameRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::CreateWorkspace(prepared) => {
            let ExpandedOperationCell::CreateWorkspace(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::CreateWorkspace, cell.id()));
            };
            let outcomes =
                collect_dispatch_outcomes!(outcomes, CreateWorkspace, OperationId::CreateWorkspace);
            workspace::CreateWorkspaceRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
        PreparedOperation::SquashLayerstack(prepared) => {
            let ExpandedOperationCell::SquashLayerstack(cell) = cell else {
                return Err(dispatch_mismatch(OperationId::SquashLayerstack, cell.id()));
            };
            let outcomes = collect_dispatch_outcomes!(
                outcomes,
                SquashLayerstack,
                OperationId::SquashLayerstack
            );
            layerstack::SquashLayerstackRuntime::evidence(prepared, cell, &outcomes, teardown)
        }
    }
}

#[must_use]
pub fn validate_operation_plan(plan: &OperationPlan) -> Vec<OperationValidationError> {
    match plan {
        OperationPlan::ExecCommand(plan) => command::validate(plan),
        OperationPlan::FileRead(plan) => files::validate_read(plan),
        OperationPlan::FileWrite(plan) => files::validate_write(plan),
        OperationPlan::FileEdit(plan) => files::validate_edit(plan),
        OperationPlan::FileBlame(plan) => files::validate_blame(plan),
        OperationPlan::CreateWorkspace(plan) => workspace::validate(plan),
        OperationPlan::SquashLayerstack(plan) => layerstack::validate(plan),
    }
}

pub fn expand_operation_plan(
    plan: &OperationPlan,
) -> Result<Vec<ExpandedOperationCell>, Vec<OperationValidationError>> {
    match plan {
        OperationPlan::ExecCommand(plan) => command::expand(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::ExecCommand)
                .collect()
        }),
        OperationPlan::FileRead(plan) => files::expand_read(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::FileRead)
                .collect()
        }),
        OperationPlan::FileWrite(plan) => files::expand_write(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::FileWrite)
                .collect()
        }),
        OperationPlan::FileEdit(plan) => files::expand_edit(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::FileEdit)
                .collect()
        }),
        OperationPlan::FileBlame(plan) => files::expand_blame(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::FileBlame)
                .collect()
        }),
        OperationPlan::CreateWorkspace(plan) => workspace::expand(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::CreateWorkspace)
                .collect()
        }),
        OperationPlan::SquashLayerstack(plan) => layerstack::expand(plan).map(|cells| {
            cells
                .into_iter()
                .map(ExpandedOperationCell::SquashLayerstack)
                .collect()
        }),
    }
}

#[cfg(test)]
mod runtime_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn failed_outcomes_preserve_every_closed_operation_identity() {
        let operations = [
            OperationId::ExecCommand,
            OperationId::FileRead,
            OperationId::FileWrite,
            OperationId::FileEdit,
            OperationId::FileBlame,
            OperationId::CreateWorkspace,
            OperationId::SquashLayerstack,
        ];

        for operation in operations {
            let outcome = OperationOutcome::failed(
                operation,
                "request-7",
                ExecutorError::RequestCancelled { operation },
            );
            assert_eq!(outcome.operation_id(), operation);
            assert_eq!(outcome.request_id(), "request-7");
            assert!(!outcome.is_success());
            assert!(outcome.response_metadata().is_none());
            match outcome.error().expect("failed outcome error") {
                ExecutorError::RequestCancelled { operation: actual } => {
                    assert_eq!(*actual, operation)
                }
                other => panic!("unexpected error: {other}"),
            }
        }
    }

    #[test]
    fn response_metadata_is_bounded_and_content_addressed() {
        let metadata = response_metadata(
            &json!({"status": "ok", "value": 7}),
            ProductOutputStatus::Succeeded,
        )
        .expect("serializable response");

        assert_eq!(metadata.status, ProductOutputStatus::Succeeded);
        assert!(metadata.response_bytes > 0);
        assert!(metadata.bounded_response_sha256.starts_with("sha256:"));
        assert_eq!(metadata.bounded_response_sha256.len(), 71);
    }
}
