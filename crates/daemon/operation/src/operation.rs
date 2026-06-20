use crate::internal::services::DaemonOperations;

pub use sandbox_protocol::{ArgCliSpec, ArgKind, ArgSpec, CliSpec, OperationFamily, OperationSpec};

pub type OperationRequest<'a> = sandbox_protocol::Request<'a>;
pub type OperationResponse = sandbox_protocol::Response;

pub type OperationDispatch = fn(&DaemonOperations, OperationRequest<'_>) -> OperationResponse;

#[derive(Clone, Copy)]
pub struct OperationEntry {
    pub spec: &'static OperationSpec,
    pub dispatch: OperationDispatch,
}

impl OperationEntry {
    #[must_use]
    pub const fn new(spec: &'static OperationSpec, dispatch: OperationDispatch) -> Self {
        Self { spec, dispatch }
    }
}
