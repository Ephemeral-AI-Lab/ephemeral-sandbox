use std::path::Path;

use crate::{
    AllocationHandle, DeletionCapability, MutableLease, OperationId, PocError, PocResult,
    SessionId, WriterCapability,
};

pub fn issue_workspace_lease(
    _allocation: &AllocationHandle,
    _session_id: SessionId,
    _operation_id: &OperationId,
) -> PocResult<MutableLease> {
    Err(PocError::Unsupported(
        "epoch-fenced lease implementation is assigned to M0 Worker B".to_owned(),
    ))
}

pub fn validate_writer(_allocation_root: &Path, _capability: &WriterCapability) -> PocResult<()> {
    Err(PocError::Unsupported(
        "epoch-fenced lease implementation is assigned to M0 Worker B".to_owned(),
    ))
}

pub fn validate_deleter(
    _allocation_root: &Path,
    _capability: &DeletionCapability,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "epoch-fenced lease implementation is assigned to M0 Worker B".to_owned(),
    ))
}
