use sandbox_runtime_mpla_poc::projection::{select_exact, MAX_RECENT_DELTAS};
use sandbox_runtime_mpla_poc::{
    AllocationId, AttributionRootId, CanonicalRootPair, ProjectionRecipe, RootId, SCHEMA_VERSION,
};

fn roots() -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse("11".repeat(32)).expect("root"),
        attribution_root_id: AttributionRootId::parse("22".repeat(32)).expect("attribution"),
    }
}

#[test]
fn exact_projection_is_zero_build_and_bounded() {
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: AllocationId::new(),
        net_delta_carrier_id: Some(AllocationId::new()),
        recent_delta_ids: (0..MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    let receipt = select_exact(&recipe).expect("exact selection");
    assert_eq!(receipt.kernel_lower_count, 10);
    assert_eq!(receipt.reconstructed_payload_bytes, 0);
    assert_eq!(receipt.hydrated_payload_bytes, 0);
    assert_eq!(receipt.base_bytes_copied, 0);
    assert!(!receipt.projection_built_during_activation);
}

#[test]
fn projection_rejects_depth_and_aliasing() {
    let base = AllocationId::new();
    let too_deep = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: (0..=MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    assert!(too_deep.validate().is_err());

    let aliased = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: vec![base],
    };
    assert!(aliased.validate().is_err());
}
