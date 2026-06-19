use std::path::Path;

use crate::commit::CommitWriter;
use crate::model::LayerChange;
use crate::test_fixture::{lp, unique_suffix, Fixture, TestResult};
use crate::{
    process_state_test_lock, reset_process_state_for_tests, service, CommitOptions, LayerStack,
    MergedView,
};

use super::{
    normalize_root_key, services, snapshot_manifest, snapshot_manifest_preserving_layer_ids,
    RootService, ServiceCache, SERVICE_CACHE_MAX,
};

fn root_service(root: &Path) -> TestResult<RootService> {
    Ok(std::sync::Arc::new(CommitWriter::with_options(
        root.to_path_buf(),
        CommitOptions::default(),
    )?))
}

fn service_cache_contains_root_for_tests(root: &Path) -> bool {
    let key = normalize_root_key(root);
    let key_prefix = format!("{key}|");
    services()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entries
        .keys()
        .any(|entry| entry == &key || entry.starts_with(&key_prefix))
}

#[test]
fn snapshot_manifest_converts_absolute_layer_paths_to_relative() -> TestResult {
    let root = std::path::PathBuf::from("/stack");
    let manifest = snapshot_manifest(&root, 7, &[root.join("layers/a"), root.join("layers/b")])?;

    assert_eq!(manifest.version, 7);
    assert_eq!(manifest.layers[0].path, "layers/a");
    assert_eq!(manifest.layers[1].path, "layers/b");
    Ok(())
}

#[test]
fn snapshot_manifest_preserving_layer_ids_uses_layer_dir_names() -> TestResult {
    let root = std::path::PathBuf::from("/stack");
    let manifest = snapshot_manifest_preserving_layer_ids(
        &root,
        7,
        &[
            root.join("layers/L000002-a"),
            root.join("layers/B000001-base"),
        ],
    )?;

    assert_eq!(manifest.layers[0].layer_id, "L000002-a");
    assert_eq!(manifest.layers[1].layer_id, "B000001-base");
    assert_eq!(manifest.layers[0].path, "layers/L000002-a");
    Ok(())
}

#[test]
fn snapshot_manifest_rejects_absolute_layer_paths_outside_root() {
    let error = snapshot_manifest(
        &std::path::PathBuf::from("/stack"),
        7,
        &[std::path::PathBuf::from("/other/layers/a")],
    )
    .expect_err("outside-root path should fail");

    assert!(
        error.to_string().contains("outside /stack"),
        "unexpected error: {error}"
    );
}

#[test]
fn commit_direct_trace_events_include_worker_handoff_and_batch_facts() -> TestResult {
    let fixture = Fixture::new("worker_handoff_trace")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let layer_paths = manifest
        .layers
        .iter()
        .map(|layer| fixture.root.join(&layer.path))
        .collect::<Vec<_>>();

    let result = service::publish_changes_to_layerstack(service::PublishChangesRequest {
        root: &fixture.root,
        snapshot_manifest_version: manifest.version,
        snapshot_layer_paths: &layer_paths,
        changes: &[LayerChange::Write {
            path: lp("README.md")?,
            content: b"# updated\n".to_vec(),
        }],
        options: CommitOptions::default(),
    })?;

    assert!(result.success());
    let events = result.trace_events();
    let handoff = events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["path_count"], 1);
    assert_eq!(handoff.details["publishable_change_count"], 1);
    assert_eq!(handoff.details["atomic"], true);
    assert_eq!(handoff.details["gated_path_count"], 1);
    assert_eq!(handoff.details["direct_path_count"], 0);
    assert_eq!(handoff.details["drop_path_count"], 0);

    let batch = events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_batch_finished")
        .expect("worker batch event");
    assert_eq!(batch.details["batch_item_count"], 1);
    assert_eq!(batch.details["combined_path_count"], 1);
    assert_eq!(batch.details["combined_change_count"], 1);
    assert_eq!(batch.details["atomic"], true);
    assert_eq!(batch.details["cas_retry_count"], 0);
    Ok(())
}

#[test]
fn process_state_reset_clears_service_cache_and_lease_registry() -> TestResult {
    let _state_guard = process_state_test_lock();
    reset_process_state_for_tests();
    let fixture = Fixture::new("process_state_reset")?;
    let _snapshot = service::acquire_snapshot_with_lease(&fixture.root, "reset-test")?;
    {
        let stack = LayerStack::open(fixture.root.clone())?;
        assert_eq!(stack.active_lease_count(), 1, "lease registry has snapshot");
    }

    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let layer_paths = manifest
        .layers
        .iter()
        .map(|layer| fixture.root.join(&layer.path))
        .collect::<Vec<_>>();
    let result = service::publish_changes_to_layerstack(service::PublishChangesRequest {
        root: &fixture.root,
        snapshot_manifest_version: manifest.version,
        snapshot_layer_paths: &layer_paths,
        changes: &[LayerChange::Write {
            path: lp("README.md")?,
            content: b"# reset\n".to_vec(),
        }],
        options: CommitOptions::default(),
    })?;
    assert!(result.success(), "commit creates a cached per-root writer");
    assert!(service_cache_contains_root_for_tests(&fixture.root));

    reset_process_state_for_tests();

    assert!(!service_cache_contains_root_for_tests(&fixture.root));
    let stack = LayerStack::open(fixture.root.clone())?;
    assert_eq!(stack.active_lease_count(), 0, "lease registry was reset");
    Ok(())
}

#[test]
fn compact_snapshot_for_remount_retargets_active_lease_to_one_layer() -> TestResult {
    let _state_guard = process_state_test_lock();
    reset_process_state_for_tests();
    let fixture = Fixture::new("compact_snapshot_retarget")?;
    for index in 0..4 {
        LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
            path: lp("large.txt")?,
            content: format!("value-{index}\n").into_bytes(),
        }])?;
    }
    let snapshot = service::acquire_snapshot_with_lease(&fixture.root, "lease-compaction")?;
    assert!(
        snapshot.layer_paths.len() > 1,
        "test requires a retained multi-layer snapshot"
    );

    let compaction = service::compact_snapshot_layers(service::CompactSnapshotLayersRequest {
        root: &fixture.root,
        snapshot_manifest_version: snapshot.manifest_version,
        snapshot_layer_paths: &snapshot.layer_paths,
    })?;
    assert_eq!(compaction.before_layer_count, snapshot.layer_paths.len());
    assert_eq!(compaction.after_layer_count, 1);
    assert_eq!(compaction.layer_paths.len(), 1);
    let (bytes, exists) =
        MergedView::new(fixture.root.clone()).read_bytes("large.txt", &compaction.manifest)?;
    assert!(exists);
    assert_eq!(bytes, Some(b"value-3\n".to_vec()));

    let mut stack = LayerStack::open(fixture.root.clone())?;
    assert_eq!(stack.active_lease_count(), 1);
    assert_eq!(stack.leased_layers().len(), snapshot.layer_paths.len());
    assert!(stack.retarget_lease_manifest(&snapshot.lease_id, compaction.manifest)?);
    assert_eq!(stack.active_lease_count(), 1, "retarget keeps lease alive");
    assert_eq!(
        stack.leased_layers().len(),
        1,
        "lease now pins only the compact checkpoint"
    );
    Ok(())
}

#[test]
fn service_cache_is_bounded_lru() -> TestResult {
    let mut cache = ServiceCache::default();
    let base = std::env::temp_dir().join(format!("eos-service-cache-{}", unique_suffix()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base)?;

    let first = base.join("root-000");
    let second = base.join("root-001");
    for index in 0..=SERVICE_CACHE_MAX {
        let root = base.join(format!("root-{index:03}"));
        std::fs::create_dir_all(&root)?;
        let _ = cache.insert_or_get(normalize_root_key(&root), root_service(&root)?);
    }

    assert_eq!(cache.entries.len(), SERVICE_CACHE_MAX);
    assert!(
        !cache.entries.contains_key(&normalize_root_key(&first)),
        "oldest root evicted"
    );
    assert!(
        cache.entries.contains_key(&normalize_root_key(&second)),
        "next-oldest root remains until another insert"
    );

    // root-000 was evicted, so re-inserting it creates again and evicts the
    // next-oldest entry instead of hitting the cache.
    let _ = cache.insert_or_get(normalize_root_key(&first), root_service(&first)?);
    assert_eq!(cache.entries.len(), SERVICE_CACHE_MAX);
    assert!(
        cache.entries.contains_key(&normalize_root_key(&first)),
        "evicted root recreated"
    );
    assert!(
        !cache.entries.contains_key(&normalize_root_key(&second)),
        "recreate evicts next oldest"
    );

    let _ = std::fs::remove_dir_all(base);
    Ok(())
}
