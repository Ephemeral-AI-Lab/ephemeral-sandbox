#![allow(
    dead_code,
    reason = "Stage 03 candidate slices are implemented in dependency order before private coordinator wiring"
)]

pub(crate) mod generation;
pub(crate) mod materialization;
pub(crate) mod materialization_operation;
pub(crate) mod native_backend;
pub(crate) mod object_store;
pub(crate) mod occ;
pub(crate) mod operation;
pub(crate) mod publication;
pub(crate) mod ref_ops;
pub(crate) mod refs;
pub(crate) mod seqcdc;
pub(crate) mod source;
pub(crate) mod spool;
pub(crate) mod tree;

impl refs::CommitLock for crate::lock::StorageWriterLockLease {
    fn with_exclusive<T, F>(&self, operation: F) -> Result<T, refs::RefError>
    where
        F: FnOnce() -> Result<T, refs::RefError>,
    {
        let _guard = self
            .exclusive()
            .map_err(|error| refs::RefError::Lock(error.to_string()))?;
        operation()
    }
}
