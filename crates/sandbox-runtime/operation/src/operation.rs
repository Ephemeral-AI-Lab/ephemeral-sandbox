use crate::internal::services::SandboxRuntimeOperations;

pub use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec, OperationCatalog,
    OperationExecutionSpace, OperationFamilySpec,
};

#[derive(Clone, Copy)]
pub(crate) struct OperationEntry {
    pub(crate) spec: &'static CliOperationSpec,
    pub(crate) dispatch:
        fn(&SandboxRuntimeOperations, &sandbox_protocol::Request) -> sandbox_protocol::Response,
}

impl OperationEntry {
    #[must_use]
    pub(crate) const fn new(
        spec: &'static CliOperationSpec,
        dispatch: fn(
            &SandboxRuntimeOperations,
            &sandbox_protocol::Request,
        ) -> sandbox_protocol::Response,
    ) -> Self {
        Self { spec, dispatch }
    }
}
