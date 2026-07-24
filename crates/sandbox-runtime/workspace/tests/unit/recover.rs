//! Boot reap through the public service surface: every persisted handle is a
//! dead session; reap destroys run dirs (containment-guarded), resets the
//! handle file, and reports what it did.

use std::path::{Path, PathBuf};

use sandbox_observability_telemetry::Observer;
use sandbox_runtime_workspace::session::{ResourceCaps, WorkspaceManager};
use sandbox_runtime_workspace::WorkspaceRuntimeService;
use serde_json::json;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::current_dir()
        .expect("workspace tests run from a current directory")
        .join("target")
        .join(format!("workspace-reap-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn service(scratch_root: &Path) -> WorkspaceRuntimeService {
    service_with_layer_root(scratch_root, scratch_root.join("layer-stack"))
}

fn service_with_layer_root(
    scratch_root: &Path,
    layer_stack_root: PathBuf,
) -> WorkspaceRuntimeService {
    let manager = WorkspaceManager::new(
        "/workspace",
        ResourceCaps::default(),
        scratch_root.to_path_buf(),
        Observer::disabled(),
    );
    WorkspaceRuntimeService::new(manager, layer_stack_root)
}

fn initialize_layer_stack(layer_stack_root: &Path) {
    let layer = layer_stack_root.join("layers/B000001-base");
    std::fs::create_dir_all(&layer).expect("base layer");
    std::fs::create_dir_all(layer_stack_root.join("staging")).expect("staging");
    std::fs::write(layer.join("README.md"), "# base\n").expect("base file");
    std::fs::write(
        layer_stack_root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "version": 1,
            "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
        }))
        .expect("manifest json"),
    )
    .expect("manifest");
}

#[test]
fn reap_destroys_run_dirs_and_resets_the_handle_file() {
    let scratch_root = scratch("basic");
    let run_dir = scratch_root.join("ws-dead");
    std::fs::create_dir_all(run_dir.join("upper")).expect("plant run dir");
    let foreign =
        std::env::temp_dir().join(format!("workspace-reap-foreign-{}", std::process::id()));
    std::fs::create_dir_all(&foreign).expect("plant foreign dir");
    let payload = json!({
        "schema_version": 1,
        "handles": [
            {"workspace_handle_id": "ws-dead", "scratch_dir": run_dir.to_string_lossy()},
            {"workspace_handle_id": "ws-escape", "scratch_dir": foreign.to_string_lossy()},
        ],
    });
    std::fs::write(
        scratch_root.join("manager.json"),
        serde_json::to_vec_pretty(&payload).expect("encode"),
    )
    .expect("write manager.json");

    let service = service(&scratch_root);
    let reaped = service.reap_persisted_sessions().expect("reap");
    assert_eq!(reaped.len(), 2);
    assert!(reaped[0].run_dir_removed, "in-scratch run dir destroyed");
    assert_eq!(reaped[0].lease_released, None, "old schema has no lease");
    assert!(reaped[0].persisted_handle_released);
    assert!(!run_dir.exists());
    assert!(
        !reaped[1].run_dir_removed,
        "paths outside the scratch root are never followed"
    );
    assert!(foreign.exists(), "foreign dir untouched");

    let rewritten: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(scratch_root.join("manager.json")).expect("read"),
    )
    .expect("parse");
    assert_eq!(
        rewritten["handles"].as_array().map(Vec::len),
        Some(0),
        "reap resets the handle file to the (empty) live set"
    );

    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&scratch_root);
}

#[test]
fn reap_releases_active_and_parked_leases_before_dropping_persisted_handle() {
    let scratch_root = scratch("leases");
    let layer_stack_root = scratch_root.join("layer-stack");
    initialize_layer_stack(&layer_stack_root);
    let active = sandbox_runtime_layerstack::service::acquire_snapshot_with_lease(
        &layer_stack_root,
        "persisted-active",
    )
    .expect("active lease");
    let parked = sandbox_runtime_layerstack::service::acquire_snapshot_with_lease(
        &layer_stack_root,
        "persisted-parked",
    )
    .expect("parked lease");
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(layer_stack_root.clone())
            .expect("stack")
            .active_lease_count(),
        2
    );

    let run_dir = scratch_root.join("ws-leased");
    std::fs::create_dir_all(run_dir.join("upper")).expect("run dir");
    let payload = json!({
        "schema_version": 1,
        "handles": [{
            "workspace_handle_id": "ws-leased",
            "scratch_dir": run_dir.to_string_lossy(),
            "lease_id": active.lease_id,
            "parked_lease_id": parked.lease_id,
        }],
    });
    std::fs::write(
        scratch_root.join("manager.json"),
        serde_json::to_vec_pretty(&payload).expect("encode"),
    )
    .expect("manager.json");

    let reaped = service_with_layer_root(&scratch_root, layer_stack_root.clone())
        .reap_persisted_sessions()
        .expect("reap");
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].lease_released, Some(true));
    assert_eq!(reaped[0].lease_release_error, None);
    assert!(reaped[0].persisted_handle_released);
    assert!(!run_dir.exists());
    assert_eq!(
        sandbox_runtime_layerstack::LayerStack::open(layer_stack_root)
            .expect("stack")
            .active_lease_count(),
        0
    );

    let _ = std::fs::remove_dir_all(&scratch_root);
}

#[test]
fn reap_retains_persisted_handle_until_failed_lease_release_can_retry() {
    let scratch_root = scratch("lease-retry");
    let layer_stack_root = scratch_root.join("blocked-layer-stack");
    std::fs::write(&layer_stack_root, "not a directory").expect("block layer root");
    let run_dir = scratch_root.join("ws-retry");
    std::fs::create_dir_all(run_dir.join("upper")).expect("run dir");
    let payload = json!({
        "schema_version": 1,
        "handles": [{
            "workspace_handle_id": "ws-retry",
            "scratch_dir": run_dir.to_string_lossy(),
            "lease_id": "lease-retry",
        }],
    });
    std::fs::write(
        scratch_root.join("manager.json"),
        serde_json::to_vec_pretty(&payload).expect("encode"),
    )
    .expect("manager.json");
    let service = service_with_layer_root(&scratch_root, layer_stack_root.clone());

    let first = service.reap_persisted_sessions().expect("first reap");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].lease_released, Some(false));
    assert!(first[0].lease_release_error.is_some());
    assert!(!first[0].persisted_handle_released);
    assert!(
        !run_dir.exists(),
        "successful scratch cleanup is not undone"
    );
    let retained: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(scratch_root.join("manager.json")).expect("retained file"),
    )
    .expect("retained json");
    assert_eq!(retained["handles"].as_array().map(Vec::len), Some(1));

    std::fs::remove_file(&layer_stack_root).expect("unblock layer root");
    let second = service.reap_persisted_sessions().expect("retry reap");
    assert_eq!(second[0].lease_released, Some(true));
    assert!(second[0].persisted_handle_released);
    let cleared: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(scratch_root.join("manager.json")).expect("cleared file"),
    )
    .expect("cleared json");
    assert_eq!(cleared["handles"].as_array().map(Vec::len), Some(0));

    let _ = std::fs::remove_dir_all(&scratch_root);
}

#[test]
fn reap_tolerates_missing_and_garbage_handle_files() {
    let scratch_root = scratch("garbage");
    let service = service(&scratch_root);
    assert!(service.reap_persisted_sessions().expect("reap").is_empty());

    std::fs::write(scratch_root.join("manager.json"), b"{not json").expect("garbage");
    assert!(service.reap_persisted_sessions().expect("reap").is_empty());
    let rewritten = std::fs::read_to_string(scratch_root.join("manager.json")).expect("read");
    assert!(
        serde_json::from_str::<serde_json::Value>(&rewritten).is_ok(),
        "an unparsable handle file is rewritten to a clean empty set"
    );
    let _ = std::fs::remove_dir_all(&scratch_root);
}
