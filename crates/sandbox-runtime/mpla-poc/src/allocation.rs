use std::path::Path;

use crate::{AllocationHandle, OperationId, PocError, PocResult};

pub fn create_allocation(
    _arena_root: &Path,
    _operation_id: &OperationId,
) -> PocResult<AllocationHandle> {
    Err(PocError::Unsupported(
        "permanent allocation implementation is assigned to M0 Worker B".to_owned(),
    ))
}

pub fn open_allocation(
    _arena_root: &Path,
    _allocation_id: &crate::AllocationId,
) -> PocResult<AllocationHandle> {
    Err(PocError::Unsupported(
        "permanent allocation implementation is assigned to M0 Worker B".to_owned(),
    ))
}
