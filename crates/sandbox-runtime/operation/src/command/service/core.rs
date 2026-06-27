use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sandbox_runtime_namespace_execution::{
    NamespaceExecutionEngine, NamespaceExecutionId, NoopObserver,
};

use crate::command::{CommandConfig, CommandExecValue};
use crate::layerstack::LayerStackService;
use crate::namespace_execution::RuntimeNamespaceExecutionSnapshot;
use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, NetworkProfile,
    WorkspaceSessionId,
};
use crate::workspace_session::{
    WorkspaceSessionError, WorkspaceSessionHandler, WorkspaceSessionService,
};

const MAX_ACTIVE_COMMANDS: usize = 256;

const COMMAND_ENGINE_SETUP_TIMEOUT_S: f64 = 30.0;

pub struct CommandOperationService {
    workspace: Arc<WorkspaceSessionService>,
    layerstack: Arc<LayerStackService>,
    config: CommandConfig,
    engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
    session_lifecycle_lock: Mutex<()>,
}

pub(crate) type SessionLifecycleGuard<'a> = MutexGuard<'a, ()>;

impl CommandOperationService {
    #[must_use]
    pub fn new(
        workspace: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
        config: CommandConfig,
    ) -> Self {
        let engine = Arc::new(NamespaceExecutionEngine::new(
            Arc::new(NoopObserver),
            MAX_ACTIVE_COMMANDS,
            COMMAND_ENGINE_SETUP_TIMEOUT_S,
        ));
        Self::with_engine(workspace, layerstack, config, engine)
    }

    /// Build a command service over a caller-supplied engine. The test harness
    /// wires that engine to a local fake launcher; production goes through `new`.
    #[doc(hidden)]
    #[must_use]
    pub fn with_engine(
        workspace: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
        config: CommandConfig,
        engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
    ) -> Self {
        Self {
            workspace,
            layerstack,
            config,
            engine,
            session_lifecycle_lock: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn active_namespace_executions(&self) -> Vec<RuntimeNamespaceExecutionSnapshot> {
        let mut snapshots = self.engine.live_values(|command| {
            Some(RuntimeNamespaceExecutionSnapshot {
                namespace_execution_id: command.exec.id().clone(),
                workspace_session_id: command.workspace_session_id.clone(),
                operation_name: command.operation_name.to_owned(),
            })
        });
        snapshots.sort_by(|left, right| {
            left.namespace_execution_id
                .cmp(&right.namespace_execution_id)
        });
        snapshots
    }

    #[must_use]
    pub fn config(&self) -> &CommandConfig {
        &self.config
    }

    #[must_use]
    pub(crate) fn engine(&self) -> &Arc<NamespaceExecutionEngine<CommandExecValue>> {
        &self.engine
    }

    pub(crate) fn lock_session_lifecycle(&self) -> SessionLifecycleGuard<'_> {
        self.session_lifecycle_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn destroy_workspace_session_with_admission(
        &self,
        workspace_session_id: WorkspaceSessionId,
        grace_s: Option<f64>,
    ) -> WorkspaceDestroyOutcome {
        let _session_lifecycle = self.lock_session_lifecycle();
        let mut active_command_session_ids = self.engine.live_values(|command| {
            (command.workspace_session_id == workspace_session_id)
                .then(|| command.exec.id().clone())
        });
        active_command_session_ids.sort();
        if !active_command_session_ids.is_empty() {
            return WorkspaceDestroyOutcome::ActiveCommands {
                active_command_session_ids,
            };
        }
        let handler = match self.workspace.resolve_session(workspace_session_id) {
            Ok(handler) => handler,
            Err(error) => return WorkspaceDestroyOutcome::Failed(error),
        };
        match self
            .workspace
            .destroy_session(handler, DestroyWorkspaceRequest { grace_s })
        {
            Ok(result) => WorkspaceDestroyOutcome::Destroyed(Box::new(result)),
            Err(error) => WorkspaceDestroyOutcome::Failed(error),
        }
    }

    pub(crate) fn resolve_workspace_session(
        &self,
        workspace_session_id: WorkspaceSessionId,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.workspace.resolve_session(workspace_session_id)
    }

    pub(super) fn create_one_shot_workspace_session(
        &self,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.workspace
            .create_workspace_session(CreateWorkspaceRequest {
                network: NetworkProfile::Shared,
            })
    }

    pub(super) fn destroy_one_shot_workspace_session(
        &self,
        handler: WorkspaceSessionHandler,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        self.workspace
            .destroy_session(handler, DestroyWorkspaceRequest::default())
    }

    pub(super) fn workspace_handle(&self) -> &Arc<WorkspaceSessionService> {
        &self.workspace
    }

    pub(super) fn layerstack_handle(&self) -> &Arc<LayerStackService> {
        &self.layerstack
    }
}

/// The result of a guarded workspace-session destroy. The command service holds
/// the session-lifecycle lock across the active-command check and the destroy,
/// so the CLI layer only formats the outcome it returns.
pub(crate) enum WorkspaceDestroyOutcome {
    ActiveCommands {
        active_command_session_ids: Vec<NamespaceExecutionId>,
    },
    Destroyed(Box<DestroyWorkspaceResult>),
    Failed(WorkspaceSessionError),
}
