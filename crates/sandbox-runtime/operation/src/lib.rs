//! Runtime application handlers and orchestration over runtime primitives.
//!
//! This crate consumes runtime declarations from the semantic catalog and has
//! no wire-protocol or presentation dependency.
#![forbid(unsafe_code)]

pub(crate) extern crate sandbox_runtime_workspace as workspace_crate;

pub mod command;
pub mod file;
pub mod layerstack;
mod namespace_execution;
mod observability;
mod operations;
mod services;
pub mod workspace_session;

pub use command::CommandOperationService;
pub use layerstack::LayerStackService;
pub use namespace_execution::{
    NamespaceExecutionId, NamespaceExecutionTerminalStatus, RuntimeNamespaceExecutionSnapshot,
};
pub use observability::{
    RuntimeObservabilitySnapshot, RuntimeOwnershipSnapshot, RuntimeOwnershipTopologySnapshot,
    RuntimeTopologyWorkspaceSnapshot, RuntimeWorkspaceSnapshot,
};
pub use sandbox_runtime_layerstack::service::{
    LayerStackResourceSnapshot, LayerStackRouteSnapshot, LayerStatus, StackObservation,
    StorageAuthority, StorageRolloutMode,
};
pub use sandbox_runtime_layerstack::{
    describe_layer_delta, LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind,
};
pub use services::{
    CommandRuntimeConfig, FileRuntimeConfig, LayerstackRuntimeConfig, NamespaceExecutionCaps,
    NamespaceExecutionRuntimeConfig, Rfc1918Egress, RuntimeShutdownFailure, RuntimeShutdownPhase,
    RuntimeShutdownReport, SandboxRuntimeConfig, SandboxRuntimeOperations, WorkloadCgroupLimits,
    WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};
pub use workspace_crate::{NetworkProfile, WorkspaceSessionId};
pub use workspace_session::WorkspaceSessionService;

#[must_use]
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_operation_contract::OperationRequest,
) -> sandbox_operation_contract::OperationResponse {
    operations::dispatch::dispatch_operation(operations, request)
}

pub fn runtime_public_handler_keys(
) -> impl Iterator<Item = (sandbox_operation_contract::OperationScopeKind, &'static str)> {
    operations::dispatch::runtime_public_handler_keys()
}

pub fn runtime_internal_handler_keys(
) -> impl Iterator<Item = (sandbox_operation_contract::OperationScopeKind, &'static str)> {
    operations::dispatch::runtime_internal_handler_keys()
}

pub fn runtime_http_only_handler_keys(
) -> impl Iterator<Item = (sandbox_operation_contract::OperationScopeKind, &'static str)> {
    operations::dispatch::runtime_http_only_handler_keys()
}
