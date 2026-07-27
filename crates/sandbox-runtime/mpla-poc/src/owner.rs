use std::path::Path;

use crate::{
    AdoptionReceipt, OwnerGeneration, OwnerTransitionRequest, PocError, PocResult,
    StableAllocationReceipt,
};

pub fn current_owner(_allocation_root: &Path) -> PocResult<OwnerGeneration> {
    Err(PocError::Unsupported(
        "durable owner implementation is assigned to M0 Worker B".to_owned(),
    ))
}

pub fn compare_and_adopt(
    _allocation_root: &Path,
    _stable: &StableAllocationReceipt,
    _request: &OwnerTransitionRequest,
) -> PocResult<AdoptionReceipt> {
    Err(PocError::Unsupported(
        "compare-and-adopt implementation is assigned to M0 Worker B".to_owned(),
    ))
}
