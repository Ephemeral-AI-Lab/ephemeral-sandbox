use std::path::PathBuf;

use sandbox_observability::{LayerBytes, LayerStackBytes};
use sandbox_runtime::{
    LayerStatus, NetworkProfile, RuntimeWorkspaceSnapshot, StackObservation, WorkspaceSessionId,
};
use sandbox_runtime_layerstack::LayerRef;
use serde_json::json;

use crate::observability::layerstack::{
    layerstack_view_value, stack_summary_value, workspace_layerstack_value,
};

fn workspace(id: &str, layer_ids: &[&str]) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(id.to_owned()),
        network: NetworkProfile::Shared,
        workspace_root: PathBuf::from("/workspace").join(id),
        upperdir: None,
        workdir: None,
        namespace_fd_count: Some(3),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(layer_ids.len()),
        layer_ids: layer_ids.iter().map(|id| (*id).to_owned()).collect(),
        cgroup_path: None,
    }
}

fn layer(id: &str, leased_by_workspaces: usize) -> LayerStatus {
    LayerStatus {
        layer: LayerRef {
            layer_id: id.to_owned(),
            path: format!("layers/{id}"),
        },
        leased_by_workspaces,
    }
}

fn bytes(id: &str, bytes: u64) -> LayerBytes {
    LayerBytes {
        layer_id: id.to_owned(),
        bytes,
    }
}

#[test]
fn layerstack_view_merges_bytes_and_derives_booked_by() {
    // §6 fixture: leases on {l2, l3} over l0..l4 (base → newest).
    let observation = StackObservation {
        manifest_version: 5,
        root_hash: "root-5".to_owned(),
        active_lease_count: 2,
        layers: vec![
            layer("l0", 0),
            layer("l1", 0),
            layer("l2", 1),
            layer("l3", 1),
            layer("l4", 0),
        ],
    };
    let disk = LayerStackBytes {
        layers: vec![
            bytes("l0", 120_000),
            bytes("l1", 84_000),
            bytes("l2", 20_000),
            bytes("l3", 20_000),
            bytes("l4", 5_000),
        ],
        total_bytes: 249_000,
    };

    let view = layerstack_view_value(&observation, &disk);

    assert_eq!(view["view"], json!("layerstack"));
    assert_eq!(view["manifest_version"], json!(5));
    assert_eq!(view["active_lease_count"], json!(2));
    assert_eq!(view["total_bytes"], json!(249_000));

    let layers = view["layers"].as_array().expect("layers array");
    assert_eq!(layers.len(), 5);

    // Bytes join by id.
    assert_eq!(layers[0]["bytes"], json!(120_000));
    assert_eq!(layers[2]["bytes"], json!(20_000));

    // leased by workspaces: only l2 and l3.
    assert_eq!(layers[2]["leased_by_workspaces"], json!(1));
    assert_eq!(layers[3]["leased_by_workspaces"], json!(1));
    assert_eq!(layers[0]["leased_by_workspaces"], json!(0));

    // booked by leased layers above (the §1 rule).
    assert_eq!(layers[0]["booked_by"], json!(["l2", "l3"]));
    assert_eq!(layers[1]["booked_by"], json!(["l2", "l3"]));
    assert_eq!(layers[2]["booked_by"], json!(["l3"]));
    assert_eq!(layers[3]["booked_by"], json!([]));
    assert_eq!(layers[4]["booked_by"], json!([]));
}

#[test]
fn layerstack_view_defaults_missing_layer_bytes_to_zero() {
    let observation = StackObservation {
        manifest_version: 1,
        root_hash: "root-1".to_owned(),
        active_lease_count: 0,
        layers: vec![layer("only", 0)],
    };
    let disk = LayerStackBytes::default();

    let view = layerstack_view_value(&observation, &disk);

    assert_eq!(view["layers"][0]["bytes"], json!(0));
    assert_eq!(view["total_bytes"], json!(0));
}

#[test]
fn stack_summary_reports_layer_count_bytes_and_leases() {
    let observation = StackObservation {
        manifest_version: 4,
        root_hash: "root-4".to_owned(),
        active_lease_count: 2,
        layers: vec![layer("l0", 0), layer("l1", 1), layer("l2", 1)],
    };
    let disk = LayerStackBytes {
        layers: vec![bytes("l0", 100), bytes("l1", 80), bytes("l2", 64)],
        total_bytes: 244,
    };

    let summary = stack_summary_value(&observation, &disk);

    assert_eq!(
        summary,
        json!({ "layer_count": 3, "layers_bytes": 244, "active_leases": 2 })
    );
}

#[test]
fn workspace_view_lists_mounts_and_sharing() {
    // ws-7 mounts l0,l1,l2; ws-9 mounts l0,l1. So l0/l1 are shared, l2 is private.
    let workspaces = vec![
        workspace("ws-7", &["l0", "l1", "l2"]),
        workspace("ws-9", &["l0", "l1"]),
    ];

    let view = workspace_layerstack_value(&workspaces, "ws-7", Some(156_000))
        .expect("ws-7 is present");

    assert_eq!(view["workspace"], json!("ws-7"));
    assert_eq!(view["upper_bytes"], json!(156_000));
    let mounts = view["mounts"].as_array().expect("mounts array");
    assert_eq!(mounts.len(), 3);
    assert_eq!(mounts[0], json!({ "layer_id": "l0", "shared_with": ["ws-9"] }));
    assert_eq!(mounts[1], json!({ "layer_id": "l1", "shared_with": ["ws-9"] }));
    assert_eq!(mounts[2], json!({ "layer_id": "l2", "shared_with": [] }));
}

#[test]
fn workspace_view_is_none_for_unknown_session() {
    let workspaces = vec![workspace("ws-7", &["l0"])];
    assert!(workspace_layerstack_value(&workspaces, "missing", None).is_none());
}
