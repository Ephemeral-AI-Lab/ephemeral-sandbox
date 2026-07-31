use sandbox_runtime_mpla_poc::{AttributionInput, RunId, SessionPhase};

pub(super) fn publication_attribution(run_id: &RunId) -> AttributionInput {
    AttributionInput {
        actor_id: "sandbox-runtime-publication".to_owned(),
        semantic_operation_id: run_id.as_str().to_owned(),
    }
}

pub(super) fn should_destroy_unpublished_allocation(
    phase: SessionPhase,
    ratified_sealing: bool,
) -> bool {
    !ratified_sealing
        && matches!(
            phase,
            SessionPhase::Open | SessionPhase::Closing | SessionPhase::RejectedBeforeAdoption
        )
}
