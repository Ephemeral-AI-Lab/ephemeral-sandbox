use std::sync::Arc;

use crate::namespace_execution::NamespaceExecutionStore;
use crate::observability::AsyncTraceSink;
use crate::workspace_remount::{ProcProcessGroupController, ProcessGroupController};
use crate::workspace_session::WorkspaceSessionService;

use super::core::CommandOperationService;
use super::launch::CommandLaunchDriver;

#[must_use]
pub fn command_service_with_launch_driver(
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    launch_driver: Arc<dyn CommandLaunchDriver>,
) -> CommandOperationService {
    CommandOperationService::from_parts(
        workspace,
        config,
        Arc::new(NamespaceExecutionStore::new()),
        launch_driver,
        Arc::new(ProcProcessGroupController),
        None,
    )
}

#[must_use]
pub fn command_service_with_launch_driver_and_remount_controller(
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    launch_driver: Arc<dyn CommandLaunchDriver>,
    remount_controller: Arc<dyn ProcessGroupController>,
) -> CommandOperationService {
    CommandOperationService::from_parts(
        workspace,
        config,
        Arc::new(NamespaceExecutionStore::new()),
        launch_driver,
        remount_controller,
        None,
    )
}

#[must_use]
pub fn command_service_with_launch_driver_and_async_trace_sink(
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    launch_driver: Arc<dyn CommandLaunchDriver>,
    async_trace_sink: Option<AsyncTraceSink>,
) -> CommandOperationService {
    CommandOperationService::from_parts(
        workspace,
        config,
        Arc::new(NamespaceExecutionStore::new()),
        launch_driver,
        Arc::new(ProcProcessGroupController),
        async_trace_sink,
    )
}

#[must_use]
pub fn command_service_with_launch_driver_namespace_store_and_async_trace_sink(
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    launch_driver: Arc<dyn CommandLaunchDriver>,
    namespace_execution: Arc<NamespaceExecutionStore>,
    async_trace_sink: Option<AsyncTraceSink>,
) -> CommandOperationService {
    CommandOperationService::from_parts(
        workspace,
        config,
        namespace_execution,
        launch_driver,
        Arc::new(ProcProcessGroupController),
        async_trace_sink,
    )
}
