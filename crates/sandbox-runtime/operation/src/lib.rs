#![forbid(unsafe_code)]

pub(crate) extern crate sandbox_runtime_workspace as workspace_crate;

mod internal;
mod operation;
mod public;

pub use internal::{layerstack, workspace_remount, workspace_session};
pub use operation::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec, OperationCatalog,
    OperationExecutionSpace, OperationFamilySpec,
};
pub use public::{cgroup_monitor, command};

pub use cgroup_monitor::CgroupMonitorOperationService;
pub use command::CommandOperationService;
pub use internal::services::{
    CgroupMonitorRuntimeConfig, CommandRuntimeConfig, Rfc1918Egress, SandboxRuntimeConfig,
    SandboxRuntimeOperations, WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};

#[must_use]
pub fn operation_specs() -> &'static [&'static CliOperationSpec] {
    public::operation_specs()
}

#[must_use]
pub fn operation_families() -> &'static [&'static OperationFamilySpec] {
    public::operation_families()
}

#[must_use]
pub fn operation_catalog() -> OperationCatalog {
    OperationCatalog::new(
        OperationExecutionSpace::Runtime,
        operation_families(),
        operation_specs(),
    )
}

#[must_use]
pub fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    public::dispatch_operation(operations, request)
}
