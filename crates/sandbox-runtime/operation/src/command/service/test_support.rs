use std::sync::Arc;

use sandbox_runtime_command::CommandExecution;
use sandbox_runtime_namespace_execution::NamespaceExecutionEngine;

use crate::namespace_execution::NamespaceExecutionLedger;
use crate::observability::AsyncTraceSink;
use crate::workspace_remount::{ProcProcessGroupController, ProcessGroupController};
use crate::workspace_session::WorkspaceSessionService;

use super::core::CommandOperationService;

/// The production `/proc`-backed remount controller, for harnesses that do not
/// inject their own (`ProcProcessGroupController` itself is crate-private).
#[must_use]
pub fn default_remount_controller() -> Arc<dyn ProcessGroupController> {
    Arc::new(ProcProcessGroupController)
}

/// Build a command service over a caller-supplied engine. The test harness wires
/// that engine to a local fake launcher; this facade only assembles service parts.
#[must_use]
pub fn command_service_from_engine(
    workspace: Arc<WorkspaceSessionService>,
    config: ::sandbox_runtime_command::CommandConfig,
    engine: Arc<NamespaceExecutionEngine<CommandExecution>>,
    namespace_execution: Arc<NamespaceExecutionLedger>,
    async_trace_sink: Option<AsyncTraceSink>,
    remount_controller: Arc<dyn ProcessGroupController>,
) -> CommandOperationService {
    CommandOperationService::from_parts(
        workspace,
        config,
        engine,
        namespace_execution,
        async_trace_sink,
        remount_controller,
    )
}
