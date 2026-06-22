use sandbox_protocol::{
    CliOperationSpec, OperationCatalog, OperationExecutionSpace, OperationFamilySpec,
};

use super::impls;

#[must_use]
pub const fn operation_families() -> &'static [&'static OperationFamilySpec] {
    impls::operation_families()
}

#[must_use]
pub const fn operation_specs() -> &'static [&'static CliOperationSpec] {
    impls::operation_specs()
}

#[must_use]
pub const fn operation_catalog() -> OperationCatalog {
    OperationCatalog::new(
        OperationExecutionSpace::Manager,
        operation_families(),
        operation_specs(),
    )
}
