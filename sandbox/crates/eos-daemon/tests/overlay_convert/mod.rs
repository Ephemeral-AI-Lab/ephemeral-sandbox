use std::path::PathBuf;

use super::*;

#[test]
fn manifest_from_snapshot_converts_absolute_layer_paths_to_relative() {
    let root = PathBuf::from("/stack");
    let manifest = manifest_from_snapshot(
        &root,
        &EphemeralSnapshot {
            lease_id: "lease-1".to_owned(),
            manifest_version: 7,
            manifest_root_hash: "hash".to_owned(),
            layer_paths: vec![root.join("layers/a"), root.join("layers/b")],
        },
    )
    .expect("snapshot manifest");

    assert_eq!(manifest.version, 7);
    assert_eq!(manifest.layers[0].path, "layers/a");
    assert_eq!(manifest.layers[1].path, "layers/b");
}

#[test]
fn manifest_from_snapshot_rejects_absolute_layer_paths_outside_root() {
    let error = manifest_from_snapshot(
        &PathBuf::from("/stack"),
        &EphemeralSnapshot {
            lease_id: "lease-1".to_owned(),
            manifest_version: 7,
            manifest_root_hash: "hash".to_owned(),
            layer_paths: vec![PathBuf::from("/other/layers/a")],
        },
    )
    .expect_err("outside-root path should fail");

    assert!(
        error.to_string().contains("outside /stack"),
        "unexpected error: {error}"
    );
}
