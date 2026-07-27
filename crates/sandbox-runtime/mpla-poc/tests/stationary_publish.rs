use sandbox_runtime_mpla_poc::{
    FaultInjector, FaultPoint, OperationId, PocError, PublicationId, StationaryPublicationRequest,
    SCHEMA_VERSION,
};

#[test]
fn stationary_publication_scope_round_trips_without_physical_identity() {
    let request = StationaryPublicationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: OperationId::from_string("operation"),
        publication_id: PublicationId::from_string("publication"),
    };
    let json = serde_json::to_string(&request).expect("serialize request");
    assert!(!json.contains("allocation_path"));
    assert!(!json.contains("inode"));
    assert_eq!(
        serde_json::from_str::<StationaryPublicationRequest>(&json).expect("deserialize request"),
        request
    );
}

#[test]
fn deterministic_faults_respect_the_terminal_sealing_boundary() {
    let mut before = FaultInjector::armed([FaultPoint::BeforeSealing]);
    assert!(matches!(
        before
            .hit(FaultPoint::BeforeSealing, false)
            .expect_err("pre-Sealing fault"),
        PocError::Integrity(_)
    ));

    let mut after = FaultInjector::armed([FaultPoint::AfterSealingDurable]);
    assert!(matches!(
        after
            .hit(FaultPoint::AfterSealingDurable, true)
            .expect_err("post-Sealing fault"),
        PocError::RecoveryRequired(_)
    ));
}
