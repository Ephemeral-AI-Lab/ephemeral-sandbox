mod impls;
mod model;
pub(crate) mod support;

pub use impls::{
    acquire_hidden_candidate_generation, acquire_hidden_candidate_generation_with_snapshot,
    acquire_snapshot_with_lease, finalize_hidden_candidate_session, get_snapshot,
    lookup_hidden_candidate_generation, materialize_hidden_candidate,
    record_hidden_candidate_mount, release_candidate_generation_lease, release_lease,
    renew_candidate_generation_lease,
};
pub use model::{
    CandidateGenerationAdmission, CandidateGenerationLease, CandidateGenerationSelection,
    CandidateMaterializationDisposition, CandidateMaterializationResult,
    LayerStackResourceSnapshot, LayerStackRouteSnapshot, LayerStatus, NativeRouteCounters,
    Snapshot, StackObservation, StorageAuthority, StorageRolloutMode, WriterLockMetrics,
};
