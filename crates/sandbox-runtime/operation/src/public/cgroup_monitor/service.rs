mod contract;
mod core;
mod error;
mod impls;
mod types;

pub use contract::{InspectCgroupMonitorInput, ReadCgroupMonitorSamplesInput};
pub use core::CgroupMonitorOperationService;
pub use error::CgroupMonitorServiceError;
pub use types::{InspectCgroupMonitorOutput, ReadCgroupMonitorSamplesOutput};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    impls::OPERATIONS
}

pub(crate) fn operation_specs() -> &'static [&'static crate::operation::CliOperationSpec] {
    impls::SPECS
}
