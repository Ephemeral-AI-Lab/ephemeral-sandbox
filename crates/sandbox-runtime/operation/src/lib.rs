#![forbid(unsafe_code)]

pub(crate) extern crate sandbox_runtime_workspace as workspace_crate;

mod cli_definition;
pub mod command;
pub mod layerstack;
mod namespace_execution;
mod observability;
mod operation;
mod services;
pub mod workspace_remount;
pub mod workspace_session;

pub use cli_definition::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationCatalog, CliOperationExecutionSpace,
    CliOperationFamilySpec, CliOperationSpec, CliSpec,
};
pub use command::CommandOperationService;
pub use layerstack::LayerStackService;
pub use namespace_execution::{
    BeginNamespaceExecution, CompleteNamespaceExecution, NamespaceExecutionId,
    NamespaceExecutionLifecycle, NamespaceExecutionRecord, NamespaceExecutionStore,
    NamespaceExecutionTerminalStatus, RuntimeNamespaceExecutionSnapshot,
};
pub use observability::{
    span_keys, AsyncTraceSink, CommandFinalizationTraceMetadata, CompletedOperationSpan,
    CompletedOperationTrace, OperationTrace, RuntimeExecutionSnapshot,
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, SpanKey,
};
pub use services::{
    CommandRuntimeConfig, Rfc1918Egress, SandboxRuntimeConfig, SandboxRuntimeOperations,
    WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};
pub use workspace_crate::{WorkspaceProfile, WorkspaceSessionId};
pub use workspace_remount::WorkspaceRemountService;
pub use workspace_session::WorkspaceSessionService;

#[must_use]
pub fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    operation::cli_operation_specs()
}

#[must_use]
pub fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    operation::cli_operation_families()
}

#[must_use]
pub fn cli_operation_catalog() -> CliOperationCatalog {
    CliOperationCatalog::new(
        CliOperationExecutionSpace::Runtime,
        cli_operation_families(),
        cli_operation_specs(),
    )
}

#[must_use]
pub fn known_operation_name(operation: &str) -> Option<&'static str> {
    operation::known_operation_name(operation)
}

#[must_use]
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response {
    operation::dispatch_operation(operations, request, trace)
}
