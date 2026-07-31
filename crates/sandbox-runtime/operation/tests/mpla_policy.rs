mod mpla_policy {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/workspace_session/service/mpla_policy.rs"
    ));
}

use mpla_policy::{publication_attribution, should_destroy_unpublished_allocation};
use sandbox_runtime_mpla_poc::{RunId, SessionPhase};

#[test]
fn publication_attribution_is_stable_across_operations_in_run() {
    let run_id = RunId::parse("attribution-stability-run").expect("parse run ID");
    let initial = publication_attribution(&run_id);
    let incremental = publication_attribution(&run_id);

    assert_eq!(initial.actor_id, "sandbox-runtime-publication");
    assert_eq!(initial.semantic_operation_id, run_id.as_str());
    assert_eq!(incremental, initial);
}

#[test]
fn ratified_sealing_prevents_unpublished_allocation_deletion() {
    for phase in [
        SessionPhase::Open,
        SessionPhase::Closing,
        SessionPhase::RejectedBeforeAdoption,
    ] {
        assert!(should_destroy_unpublished_allocation(phase, false));
        assert!(!should_destroy_unpublished_allocation(phase, true));
    }
    for phase in [
        SessionPhase::Sealing,
        SessionPhase::PublicationCommitted,
        SessionPhase::RecoveryRequired,
    ] {
        assert!(!should_destroy_unpublished_allocation(phase, false));
        assert!(!should_destroy_unpublished_allocation(phase, true));
    }
}
