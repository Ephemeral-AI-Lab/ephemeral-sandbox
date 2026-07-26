#[expect(
    dead_code,
    reason = "the candidate generation contract is also exercised through external conformance tests"
)]
pub(crate) mod generation;
#[expect(
    dead_code,
    reason = "the candidate materialization contract retains externally exercised coordinator entry points"
)]
pub(crate) mod materialization;
#[expect(
    dead_code,
    reason = "the typed operation contract is also exercised through external conformance tests"
)]
pub(crate) mod materialization_operation;
pub(crate) mod materialization_publication;
#[expect(
    dead_code,
    reason = "platform capability branches are externally exercised beyond the host runtime path"
)]
pub(crate) mod native_backend;
#[expect(
    dead_code,
    reason = "the candidate object contract is also exercised through external conformance tests"
)]
pub(crate) mod object_store;
#[expect(
    dead_code,
    reason = "the Stage 03 OCC contract remains externally exercised while later runtime wiring is staged"
)]
pub(crate) mod occ;
#[expect(
    dead_code,
    reason = "the candidate operation journal is also exercised through external conformance tests"
)]
pub(crate) mod operation;
pub(crate) mod publication;
#[expect(
    dead_code,
    reason = "the Stage 03 ref-operation contract remains externally exercised while later runtime wiring is staged"
)]
pub(crate) mod ref_ops;
#[expect(
    dead_code,
    reason = "the candidate ref contract is also exercised through external conformance tests"
)]
pub(crate) mod refs;
#[expect(
    dead_code,
    reason = "the deterministic chunking target is externally verified as part of the format contract"
)]
pub(crate) mod seqcdc;
#[expect(
    dead_code,
    reason = "the Stage 03 source-protection contract remains externally exercised while later runtime wiring is staged"
)]
pub(crate) mod source;
#[expect(
    dead_code,
    reason = "the candidate spool contract retains externally exercised diagnostic accessors"
)]
pub(crate) mod spool;
pub(crate) mod squash;
#[expect(
    dead_code,
    reason = "the candidate tree contract retains externally exercised query and diagnostic entry points"
)]
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
