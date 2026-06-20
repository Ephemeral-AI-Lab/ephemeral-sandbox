use crate::OperationSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAuthority {
    SandboxManager,
    SandboxDaemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationCatalog {
    pub authority: OperationAuthority,
    pub operations: &'static [&'static OperationSpec],
}

impl OperationCatalog {
    #[must_use]
    pub const fn new(
        authority: OperationAuthority,
        operations: &'static [&'static OperationSpec],
    ) -> Self {
        Self {
            authority,
            operations,
        }
    }
}
