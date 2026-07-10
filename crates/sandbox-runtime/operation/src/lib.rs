#![forbid(unsafe_code)]

pub(crate) extern crate sandbox_runtime_workspace as workspace_crate;

pub mod command;
pub mod file;
pub mod layerstack;
mod namespace_execution;
mod observability;
mod operation;
mod operation_adapter;
mod services;
pub mod workspace_session;

pub use command::CommandOperationService;
pub use layerstack::{ClaimedExportStream, LayerStackService};
pub use namespace_execution::{
    NamespaceExecutionId, NamespaceExecutionTerminalStatus, RuntimeNamespaceExecutionSnapshot,
};
pub use observability::{RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot};
pub use sandbox_runtime_layerstack::service::{LayerStatus, StackObservation};
pub use sandbox_runtime_layerstack::{
    describe_layer_delta, LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind,
};
pub use services::{
    LayerstackRuntimeConfig, NamespaceExecutionRuntimeConfig, Rfc1918Egress, SandboxRuntimeConfig,
    SandboxRuntimeOperations, WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};
pub use workspace_crate::{NetworkProfile, WorkspaceSessionId};
pub use workspace_session::WorkspaceSessionService;

#[must_use]
pub fn known_operation_name(operation: &str) -> Option<&'static str> {
    operation::known_operation_name(operation)
}

#[must_use]
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    operation::dispatch_operation(operations, request)
}
