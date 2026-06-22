use super::dispatch::ManagerOperationEntry;
use sandbox_protocol::{CliOperationSpec, OperationFamilySpec};

mod management;

pub(crate) const fn operation_families() -> &'static [&'static OperationFamilySpec] {
    management::operation_families()
}

pub(crate) const fn operation_specs() -> &'static [&'static CliOperationSpec] {
    management::operation_specs()
}

pub(crate) fn operation_entries() -> &'static [ManagerOperationEntry] {
    management::operation_entries()
}
