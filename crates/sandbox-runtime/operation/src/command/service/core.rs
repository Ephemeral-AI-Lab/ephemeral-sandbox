use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::command::{
    CommandLaunchDriver, CommandProcessStore, CommandSessionId, RealCommandLaunchDriver,
};
use crate::namespace_execution::{NamespaceExecutionId, NamespaceExecutionStore};
use crate::observability::AsyncTraceSink;
use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceProfile,
    WorkspaceSessionId,
};
use crate::workspace_remount::{ProcProcessGroupController, ProcessGroupController};
use crate::workspace_session::{
    WorkspaceSessionError, WorkspaceSessionHandler, WorkspaceSessionService,
};

use super::completion::{spawn_completion_finalizer, CommandCompletionSender};

pub struct CommandOperationService {
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    process_store: Arc<CommandProcessStore>,
    namespace_execution: Arc<NamespaceExecutionStore>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
    completion_sender: CommandCompletionSender,
    remount_controller: Arc<dyn ProcessGroupController>,
    workspace_lifecycle_admission: Mutex<()>,
}

pub(crate) struct WorkspaceLifecycleAdmission<'a> {
    _guard: MutexGuard<'a, ()>,
}

impl CommandOperationService {
    #[must_use]
    pub fn new(
        workspace: Arc<WorkspaceSessionService>,
        config: ::sandbox_runtime_command::CommandConfig,
    ) -> Self {
        Self::from_parts(
            workspace,
            config,
            Arc::new(NamespaceExecutionStore::new()),
            Arc::new(RealCommandLaunchDriver),
            Arc::new(ProcProcessGroupController),
            None,
        )
    }

    #[must_use]
    pub(crate) fn new_with_async_trace_sink(
        workspace: Arc<WorkspaceSessionService>,
        config: ::sandbox_runtime_command::CommandConfig,
        namespace_execution: Arc<NamespaceExecutionStore>,
        async_trace_sink: Option<AsyncTraceSink>,
    ) -> Self {
        Self::from_parts(
            workspace,
            config,
            namespace_execution,
            Arc::new(RealCommandLaunchDriver),
            Arc::new(ProcProcessGroupController),
            async_trace_sink,
        )
    }

    pub(super) fn from_parts(
        workspace: Arc<WorkspaceSessionService>,
        config: ::sandbox_runtime_command::CommandConfig,
        namespace_execution: Arc<NamespaceExecutionStore>,
        launch_driver: Arc<dyn CommandLaunchDriver>,
        remount_controller: Arc<dyn ProcessGroupController>,
        async_trace_sink: Option<AsyncTraceSink>,
    ) -> Self {
        let process_store = Arc::new(CommandProcessStore::new());
        let completion_sender = spawn_completion_finalizer(
            Arc::clone(&workspace),
            Arc::clone(&process_store),
            Arc::clone(&namespace_execution),
            async_trace_sink,
        );
        Self {
            workspace,
            config,
            process_store,
            namespace_execution,
            launch_driver,
            completion_sender,
            remount_controller,
            workspace_lifecycle_admission: Mutex::new(()),
        }
    }

    #[must_use]
    pub(crate) fn shares_workspace_session(
        &self,
        workspace: &Arc<WorkspaceSessionService>,
    ) -> bool {
        Arc::ptr_eq(&self.workspace, workspace)
    }

    #[must_use]
    pub(crate) fn shares_namespace_execution_store(
        &self,
        namespace_execution: &Arc<NamespaceExecutionStore>,
    ) -> bool {
        Arc::ptr_eq(&self.namespace_execution, namespace_execution)
    }

    #[must_use]
    pub fn namespace_execution_store(&self) -> &Arc<NamespaceExecutionStore> {
        &self.namespace_execution
    }

    #[must_use]
    pub fn config(&self) -> &::sandbox_runtime_command::CommandConfig {
        &self.config
    }

    #[must_use]
    pub(crate) fn process_store(&self) -> &Arc<CommandProcessStore> {
        &self.process_store
    }

    #[doc(hidden)]
    pub fn namespace_execution_id_for_command_for_test(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Option<NamespaceExecutionId> {
        self.process_store
            .active(command_session_id)
            .map(|active| active.namespace_execution_id.clone())
            .or_else(|| {
                self.process_store
                    .completed(command_session_id)
                    .map(|completed| completed.namespace_execution_id)
            })
    }

    #[must_use]
    pub(crate) fn launch_driver(&self) -> &Arc<dyn CommandLaunchDriver> {
        &self.launch_driver
    }

    #[must_use]
    pub(crate) fn completion_sender(&self) -> &CommandCompletionSender {
        &self.completion_sender
    }

    #[must_use]
    pub(crate) fn remount_controller(&self) -> Arc<dyn ProcessGroupController> {
        Arc::clone(&self.remount_controller)
    }

    pub(crate) fn begin_workspace_lifecycle_admission(&self) -> WorkspaceLifecycleAdmission<'_> {
        let guard = self
            .workspace_lifecycle_admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        WorkspaceLifecycleAdmission { _guard: guard }
    }

    pub(crate) fn with_workspace_destroy_admission<R>(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        dispatch: impl FnOnce(&[CommandSessionId]) -> R,
    ) -> R {
        let _lifecycle_admission = self.begin_workspace_lifecycle_admission();
        let active_command_session_ids = self
            .process_store()
            .active_command_session_ids_for_workspace_session(workspace_session_id);
        dispatch(&active_command_session_ids)
    }

    pub(super) fn resolve_workspace_session(
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
                profile: WorkspaceProfile::HostCompatible,
            })
    }

    pub(super) fn destroy_one_shot_workspace_session(
        &self,
        handler: WorkspaceSessionHandler,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        self.workspace
            .destroy_session(handler, DestroyWorkspaceRequest::default())
    }

    pub(super) fn workspace_remount_pending(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> bool {
        self.workspace.is_remount_pending(workspace_session_id)
    }

    pub(super) fn workspace_remount_blocked(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> bool {
        self.workspace.is_remount_blocked(workspace_session_id)
    }
}
