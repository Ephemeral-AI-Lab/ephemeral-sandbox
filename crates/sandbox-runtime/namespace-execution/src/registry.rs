/// Live + completed executions keyed by `NamespaceExecutionId`, with admission.
/// Phase 1: capacity placeholder only — the maps, id lookup, and `try_reserve`
/// land in Phase 2/3.
pub struct ExecutionRegistry {
    max_active: usize,
}

impl ExecutionRegistry {
    pub fn new(max_active: usize) -> Self {
        Self { max_active }
    }

    pub fn max_active(&self) -> usize {
        self.max_active
    }
}
