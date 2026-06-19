use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use layerstack::MANIFEST_SCHEMA_VERSION;
use layerstack::{
    build_workspace_base, ensure_workspace_base, LayerChange, LayerPath, LayerRef, LayerStack,
    LayerStackError, Manifest, MergedView, WorkspaceBinding, ACTIVE_MANIFEST_FILE,
    WORKSPACE_BINDING_FILE,
};
use serde_json::json;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn squash_coalesces_layers_and_preserves_merged_reads() -> TestResult {
    let fixture = Fixture::new("squash_basic");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "a.txt", "three\n")?;

    assert!(stack.can_squash(2)?);
    let squashed = stack
        .squash(2)?
        .manifest
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;

    assert_eq!(squashed.layers.len(), 1);
    assert_eq!(stack.read_text("a.txt")?.0, "three\n");
    assert_eq!(stack.read_text("b.txt")?.0, "two\n");
    assert!(stack.squash(2)?.manifest.is_none());
    Ok(())
}

#[test]
fn release_lease_gcs_squashed_layers_after_retaining_lease_drops() -> TestResult {
    let fixture = Fixture::new("squash_gc");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "c.txt", "three\n")?;

    let lease = stack.acquire_snapshot("reader")?;
    let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
    let squashed = stack
        .squash(2)?
        .manifest
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
    assert_eq!(squashed.layers.len(), 2);
    for layer in &old_tail {
        assert!(fixture.root.join(&layer.path).exists());
    }

    assert!(stack.release_lease(&lease.lease_id)?);
    for layer in &old_tail {
        assert!(!fixture.root.join(&layer.path).exists());
    }
    Ok(())
}

#[test]
fn cross_instance_lease_retains_squashed_layers_until_reopened_release() -> TestResult {
    let fixture = Fixture::new("squash_gc_cross_instance");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "c.txt", "three\n")?;
    drop(stack);

    let lease_stack = LayerStack::open(fixture.root.clone())?;
    let lease = lease_stack.acquire_snapshot("reader")?;
    let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
    assert_eq!(
        LayerStack::open(fixture.root.clone())?.active_lease_count(),
        1
    );

    let mut squash_stack = LayerStack::open(fixture.root.clone())?;
    let squashed = squash_stack
        .squash(2)?
        .manifest
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
    assert_eq!(squashed.layers.len(), 2);
    for layer in &old_tail {
        assert!(fixture.root.join(&layer.path).exists());
    }

    assert!(LayerStack::open(fixture.root.clone())?.release_lease(&lease.lease_id)?);
    assert_eq!(
        LayerStack::open(fixture.root.clone())?.active_lease_count(),
        0
    );
    for layer in &old_tail {
        assert!(!fixture.root.join(&layer.path).exists());
    }
    Ok(())
}

#[test]
fn lease_aware_view_reclaim_compacts_same_file_gap_around_single_protected_layer() -> TestResult {
    let fixture = Fixture::new("lease_aware_view_reclaim_l4");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 1..=6 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let active = stack.read_active_manifest()?;
    let original_layers = active.layers.clone();
    let protected_l4 = active.layers[2].clone();
    let lease = stack.acquire_snapshot("protect-l4-only")?;
    let protected_manifest = Manifest::new(
        active.version,
        vec![protected_l4.clone()],
        MANIFEST_SCHEMA_VERSION,
    )?;
    assert!(stack.retarget_lease_manifest(&lease.lease_id, protected_manifest)?);

    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 6 << 20);
    let outcome = stack.reclaim_lease_aware_view_checkpoints(2)?;

    let manifest = outcome.manifest.expect("view checkpoints should commit");
    assert_eq!(outcome.planned_reclaiming_interval_count, 2);
    assert_eq!(outcome.view_checkpoint_count, 2);
    assert_eq!(outcome.skipped_delta_interval_count, 0);
    assert_eq!(outcome.removed_layer_count, 5);
    assert_eq!(outcome.active_depth_before, 6);
    assert_eq!(outcome.active_depth_after, 3);
    assert_eq!(manifest.layers.len(), 3);
    assert_eq!(manifest.layers[1], protected_l4);
    assert_eq!(
        stack.read_bytes("blob.bin")?.0.unwrap_or_default().len(),
        1 << 20
    );
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 3 << 20);
    assert!(fixture.root.join(&manifest.layers[1].path).exists());
    for layer in original_layers
        .iter()
        .filter(|layer| **layer != manifest.layers[1])
    {
        assert!(
            !fixture.root.join(&layer.path).exists(),
            "{} should be reclaimed",
            layer.layer_id
        );
    }

    assert!(stack.release_lease(&lease.lease_id)?);
    let after_release = stack
        .squash(1)?
        .manifest
        .expect("final squash should collapse released l4 chain");
    assert_eq!(after_release.layers.len(), 1);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    Ok(())
}

#[test]
fn lease_aware_parent_prefix_compaction_keeps_live_l4_lease_but_reclaims_prefix() -> TestResult {
    let fixture = Fixture::new("lease_aware_live_l4_parent_prefix");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 1..=4 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let lease = stack.acquire_snapshot("running-command-at-l4")?;
    let original_lease_layers = lease.manifest.layers.clone();
    assert_eq!(original_lease_layers.len(), 4);
    for index in 5..=6 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }

    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 6 << 20);
    let normalized = stack.compact_leased_parent_for_remount(&lease.lease_id, 2)?;

    let lease_manifest = normalized
        .lease_manifest
        .as_ref()
        .expect("parent prefix compaction should retarget the live lease");
    assert_eq!(normalized.compacted_parent_layer_count, 3);
    assert_eq!(normalized.removed_layer_count, 3);
    assert_eq!(normalized.bytes_added, 1 << 20);
    assert_eq!(normalized.lease_depth_before, 4);
    assert_eq!(normalized.lease_depth_after, 2);
    assert_eq!(normalized.active_depth_before, 6);
    assert_eq!(normalized.active_depth_after, 4);
    assert_eq!(lease_manifest.layers[0], original_lease_layers[0]);
    assert_ne!(lease_manifest.layers[1], original_lease_layers[1]);
    assert_eq!(stack.active_lease_count(), 1);
    assert_eq!(stack.leased_layers().len(), 2);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 4 << 20);
    for layer in &original_lease_layers[1..] {
        assert!(
            !fixture.root.join(&layer.path).exists(),
            "{} parent prefix layer should be reclaimed after lease retarget",
            layer.layer_id
        );
    }
    let (lease_bytes, exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", lease_manifest)?;
    assert!(exists);
    assert_eq!(lease_bytes.unwrap_or_default()[0], 4);
    assert_eq!(stack.read_bytes("blob.bin")?.0.unwrap_or_default()[0], 6);

    let reclaimed = stack.reclaim_lease_aware_view_checkpoints(2)?;

    assert_eq!(reclaimed.view_checkpoint_count, 1);
    assert_eq!(reclaimed.removed_layer_count, 2);
    assert_eq!(reclaimed.active_depth_before, 4);
    assert_eq!(reclaimed.active_depth_after, 3);
    assert_eq!(stack.active_lease_count(), 1);
    assert_eq!(stack.leased_layers().len(), 2);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 3 << 20);

    assert!(stack.release_lease(&lease.lease_id)?);
    let final_manifest = stack
        .squash(1)?
        .manifest
        .expect("final squash should collapse normalized l4 chain");
    assert_eq!(final_manifest.layers.len(), 1);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    Ok(())
}

#[test]
fn lease_aware_large_parent_prefix_compaction_preserves_large_file_integrity() -> TestResult {
    let fixture = Fixture::new("lease_aware_large_l4_parent_prefix");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    let large_file_bytes = 4 << 20;
    let large_file_bytes_u64 = large_file_bytes as u64;
    for index in 1..=4 {
        publish_blob(&mut stack, "large.bin", large_file_bytes, index)?;
    }
    let lease = stack.acquire_snapshot("running-command-large-l4")?;
    for index in 5..=6 {
        publish_blob(&mut stack, "large.bin", large_file_bytes, index)?;
    }

    assert_eq!(
        payload_bytes(&fixture.root.join("layers"))?,
        6 * large_file_bytes_u64
    );
    let normalized = stack.compact_leased_parent_for_remount(&lease.lease_id, 2)?;
    let lease_manifest = normalized
        .lease_manifest
        .as_ref()
        .expect("large parent prefix should be compacted");

    assert_eq!(normalized.compacted_parent_layer_count, 3);
    assert_eq!(normalized.removed_layer_count, 3);
    assert_eq!(normalized.bytes_added, large_file_bytes as u64);
    assert_eq!(
        payload_bytes(&fixture.root.join("layers"))?,
        4 * large_file_bytes_u64
    );
    let (lease_bytes, exists) =
        MergedView::new(fixture.root.clone()).read_bytes("large.bin", lease_manifest)?;
    assert!(exists);
    let lease_bytes = lease_bytes.unwrap_or_default();
    assert_eq!(lease_bytes.len(), large_file_bytes);
    assert_eq!(lease_bytes[0], 4);
    let active_bytes = stack.read_bytes("large.bin")?.0.unwrap_or_default();
    assert_eq!(active_bytes.len(), large_file_bytes);
    assert_eq!(active_bytes[0], 6);

    let reclaimed = stack.reclaim_lease_aware_view_checkpoints(2)?;
    assert_eq!(reclaimed.view_checkpoint_count, 1);
    assert_eq!(reclaimed.removed_layer_count, 2);
    assert_eq!(
        payload_bytes(&fixture.root.join("layers"))?,
        3 * large_file_bytes_u64
    );

    assert!(stack.release_lease(&lease.lease_id)?);
    let final_manifest = stack
        .squash(1)?
        .manifest
        .expect("large final squash should collapse normalized chain");
    assert_eq!(final_manifest.layers.len(), 1);
    assert_eq!(
        payload_bytes(&fixture.root.join("layers"))?,
        large_file_bytes_u64
    );
    Ok(())
}

#[test]
fn lease_aware_multi_lease_parent_normalization_reclaims_only_unpinned_layers() -> TestResult {
    let fixture = Fixture::new("lease_aware_multi_lease_parent_prefix");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 1..=4 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let old_lease = stack.acquire_snapshot("old-running-command")?;
    let old_layers = old_lease.manifest.layers.clone();
    for index in 5..=8 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let mid_lease = stack.acquire_snapshot("mid-running-command")?;
    let mid_layers = mid_lease.manifest.layers.clone();
    for index in 9..=12 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }

    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 12 << 20);
    let normalized = stack.compact_leased_parent_for_remount(&mid_lease.lease_id, 2)?;
    let mid_manifest = normalized
        .lease_manifest
        .as_ref()
        .expect("mid lease should be retargeted to compact parent");

    assert_eq!(normalized.compacted_parent_layer_count, 7);
    assert_eq!(normalized.removed_layer_count, 3);
    assert_eq!(normalized.bytes_added, 1 << 20);
    assert_eq!(normalized.lease_depth_before, 8);
    assert_eq!(normalized.lease_depth_after, 2);
    assert_eq!(normalized.active_depth_before, 12);
    assert_eq!(normalized.active_depth_after, 6);
    assert_eq!(stack.active_lease_count(), 2);
    assert_eq!(stack.leased_layers().len(), 6);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 10 << 20);

    for layer in &mid_layers[1..4] {
        assert!(
            !fixture.root.join(&layer.path).exists(),
            "{} should be reclaimed because no lease still pins it",
            layer.layer_id
        );
    }
    for layer in &old_layers {
        assert!(
            fixture.root.join(&layer.path).exists(),
            "{} should remain pinned by the older lease",
            layer.layer_id
        );
    }

    let (old_bytes, old_exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", &old_lease.manifest)?;
    assert!(old_exists);
    assert_eq!(old_bytes.unwrap_or_default()[0], 4);
    let (mid_bytes, mid_exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", mid_manifest)?;
    assert!(mid_exists);
    assert_eq!(mid_bytes.unwrap_or_default()[0], 8);
    assert_eq!(stack.read_bytes("blob.bin")?.0.unwrap_or_default()[0], 12);

    let reclaimed = stack.reclaim_lease_aware_view_checkpoints(2)?;
    assert_eq!(reclaimed.view_checkpoint_count, 1);
    assert_eq!(reclaimed.removed_layer_count, 4);
    assert_eq!(reclaimed.active_depth_before, 6);
    assert_eq!(reclaimed.active_depth_after, 3);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 7 << 20);

    assert!(stack.release_lease(&mid_lease.lease_id)?);
    let squashed_with_old_lease = stack
        .squash(1)?
        .manifest
        .expect("active chain should squash even while historical old lease remains");
    assert_eq!(squashed_with_old_lease.layers.len(), 1);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 5 << 20);

    assert!(stack.release_lease(&old_lease.lease_id)?);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    Ok(())
}

#[test]
fn lease_aware_many_historical_leases_reclaim_top_gap_and_preserve_snapshots() -> TestResult {
    let fixture = Fixture::new("lease_aware_many_historical_leases");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 1..=4 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let lease_4 = stack.acquire_snapshot("historical-command-v4")?;
    for index in 5..=8 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let lease_8 = stack.acquire_snapshot("historical-command-v8")?;
    for index in 9..=12 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let lease_12 = stack.acquire_snapshot("historical-command-v12")?;
    for index in 13..=20 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }

    assert_eq!(stack.active_lease_count(), 3);
    assert_eq!(stack.leased_layers().len(), 12);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 20 << 20);

    let reclaimed = stack.reclaim_lease_aware_view_checkpoints(2)?;
    assert_eq!(reclaimed.planned_reclaiming_interval_count, 1);
    assert_eq!(reclaimed.view_checkpoint_count, 1);
    assert_eq!(reclaimed.removed_layer_count, 8);
    assert_eq!(reclaimed.protected_layer_count, 12);
    assert_eq!(reclaimed.active_depth_before, 20);
    assert_eq!(reclaimed.active_depth_after, 13);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 13 << 20);

    let (lease_4_bytes, lease_4_exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", &lease_4.manifest)?;
    assert!(lease_4_exists);
    assert_eq!(lease_4_bytes.unwrap_or_default()[0], 4);
    let (lease_8_bytes, lease_8_exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", &lease_8.manifest)?;
    assert!(lease_8_exists);
    assert_eq!(lease_8_bytes.unwrap_or_default()[0], 8);
    let (lease_12_bytes, lease_12_exists) =
        MergedView::new(fixture.root.clone()).read_bytes("blob.bin", &lease_12.manifest)?;
    assert!(lease_12_exists);
    assert_eq!(lease_12_bytes.unwrap_or_default()[0], 12);
    assert_eq!(stack.read_bytes("blob.bin")?.0.unwrap_or_default()[0], 20);

    assert!(stack.release_lease(&lease_12.lease_id)?);
    assert!(stack.release_lease(&lease_8.lease_id)?);
    assert!(stack.release_lease(&lease_4.lease_id)?);
    let final_manifest = stack
        .squash(1)?
        .manifest
        .expect("final squash should collapse after all historical leases release");
    assert_eq!(final_manifest.layers.len(), 1);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    Ok(())
}

#[test]
fn lease_aware_view_reclaim_skips_delete_gap_until_delta_checkpoint() -> TestResult {
    let fixture = Fixture::new("lease_aware_view_reclaim_delete_skip");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_blob(&mut stack, "a.txt", 1 << 20, 1)?;
    let lease = stack.acquire_snapshot("protect-lower-file")?;
    stack.publish_layer(&[LayerChange::Delete {
        path: LayerPath::parse("a.txt")?,
    }])?;

    assert_eq!(stack.read_text("a.txt")?, (String::new(), false));
    let before = stack.read_active_manifest()?;
    let before_payload = payload_bytes(&fixture.root.join("layers"))?;
    let outcome = stack.reclaim_lease_aware_view_checkpoints(1)?;

    assert!(outcome.manifest.is_none());
    assert_eq!(outcome.planned_reclaiming_interval_count, 1);
    assert_eq!(outcome.view_checkpoint_count, 0);
    assert_eq!(outcome.skipped_delta_interval_count, 1);
    assert_eq!(outcome.removed_layer_count, 0);
    assert_eq!(stack.read_active_manifest()?, before);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, before_payload);
    assert!(fixture.root.join(&lease.manifest.layers[0].path).exists());
    assert_eq!(stack.read_text("a.txt")?, (String::new(), false));
    Ok(())
}

#[test]
fn lease_aware_delta_reclaim_preserves_delete_above_protected_lower_file() -> TestResult {
    let fixture = Fixture::new("lease_aware_delta_delete");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_blob(&mut stack, "a.txt", 1 << 20, 1)?;
    let lease = stack.acquire_snapshot("protect-lower-file")?;
    stack.publish_layer(&[LayerChange::Delete {
        path: LayerPath::parse("a.txt")?,
    }])?;

    assert_eq!(stack.read_text("a.txt")?, (String::new(), false));
    let before_payload = payload_bytes(&fixture.root.join("layers"))?;
    let outcome = stack.reclaim_lease_aware_checkpoints(1)?;

    let manifest = outcome.manifest.expect("delta checkpoint should commit");
    assert_eq!(outcome.view_checkpoint_count, 0);
    assert_eq!(outcome.delta_checkpoint_count, 1);
    assert_eq!(outcome.skipped_delta_interval_count, 0);
    assert_eq!(outcome.removed_layer_count, 1);
    assert_eq!(manifest.layers.len(), 2);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, before_payload);
    assert_eq!(stack.read_text("a.txt")?, (String::new(), false));

    assert!(stack.release_lease(&lease.lease_id)?);
    stack.squash(1)?;
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 0);
    assert_eq!(stack.read_text("a.txt")?, (String::new(), false));
    Ok(())
}

#[test]
fn lease_aware_delta_reclaim_preserves_opaque_dir_above_protected_lower_entries() -> TestResult {
    let fixture = Fixture::new("lease_aware_delta_opaque");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_blob(&mut stack, "dir/protected.txt", 1 << 20, 1)?;
    let lease = stack.acquire_snapshot("protect-lower-dir")?;
    publish_blob(&mut stack, "dir/old-unleased.txt", 1 << 20, 2)?;
    stack.publish_layer(&[LayerChange::OpaqueDir {
        path: LayerPath::parse("dir")?,
    }])?;

    assert_eq!(
        stack.read_text("dir/protected.txt")?,
        (String::new(), false)
    );
    assert_eq!(
        stack.read_text("dir/old-unleased.txt")?,
        (String::new(), false)
    );
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 2 << 20);

    let outcome = stack.reclaim_lease_aware_checkpoints(2)?;

    let manifest = outcome.manifest.expect("delta checkpoint should commit");
    assert_eq!(outcome.view_checkpoint_count, 0);
    assert_eq!(outcome.delta_checkpoint_count, 1);
    assert_eq!(outcome.skipped_delta_interval_count, 0);
    assert_eq!(outcome.removed_layer_count, 2);
    assert_eq!(manifest.layers.len(), 2);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    assert_eq!(
        stack.read_text("dir/protected.txt")?,
        (String::new(), false)
    );
    assert_eq!(
        stack.read_text("dir/old-unleased.txt")?,
        (String::new(), false)
    );

    assert!(stack.release_lease(&lease.lease_id)?);
    stack.squash(1)?;
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 0);
    assert_eq!(
        stack.read_text("dir/protected.txt")?,
        (String::new(), false)
    );
    Ok(())
}

#[test]
fn lease_aware_copy_through_reports_pinned_bytes_without_reclaiming_protected_layers() -> TestResult
{
    let fixture = Fixture::new("lease_aware_copy_through");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 1..=6 {
        publish_blob(&mut stack, "blob.bin", 1 << 20, index)?;
    }
    let lease = stack.acquire_snapshot("protect-full-stack")?;
    let protected_layers = lease.manifest.layers.clone();

    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 6 << 20);
    let outcome = stack.copy_through_active_for_depth_guard(1)?;

    let manifest = outcome.manifest.expect("copy-through should commit");
    assert_eq!(outcome.checkpoint_count, 1);
    assert_eq!(outcome.protected_layer_count, 6);
    assert_eq!(outcome.removed_layer_count, 0);
    assert_eq!(outcome.bytes_added, 1 << 20);
    assert_eq!(outcome.protected_pinned_bytes, 6 << 20);
    assert_eq!(outcome.active_depth_before, 6);
    assert_eq!(outcome.active_depth_after, 1);
    assert_eq!(manifest.layers.len(), 1);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 7 << 20);
    for layer in &protected_layers {
        assert!(
            fixture.root.join(&layer.path).exists(),
            "{} should remain pinned",
            layer.layer_id
        );
    }

    assert!(stack.release_lease(&lease.lease_id)?);
    assert_eq!(payload_bytes(&fixture.root.join("layers"))?, 1 << 20);
    Ok(())
}

#[test]
fn delete_layer_hides_files_in_reads_and_projection() -> TestResult {
    let fixture = Fixture::new("delete_hides");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "dir/a.txt", "one\n")?;
    publish_text(&mut stack, "dir/b.txt", "two\n")?;

    stack.publish_layer(&[LayerChange::Delete {
        path: LayerPath::parse("dir/a.txt")?,
    }])?;

    assert_eq!(stack.read_text("dir/a.txt")?, (String::new(), false));
    assert_eq!(stack.read_text("dir/b.txt")?, ("two\n".to_owned(), true));

    std::fs::create_dir_all(&fixture.workspace)?;
    let _ = stack.commit_to_workspace(&fixture.workspace)?;
    assert!(!fixture.workspace.join("dir/a.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("dir/b.txt"))?,
        "two\n"
    );
    assert!(
        !fixture.workspace.join("dir/.wh.a.txt").exists(),
        "logical whiteout marker must not leak into projections"
    );
    Ok(())
}

#[test]
fn read_bytes_limited_rejects_oversized_file() -> TestResult {
    let fixture = Fixture::new("read_bytes_limited");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "large.txt", "abcdef")?;

    let error = stack
        .read_bytes_limited("large.txt", 2)
        .expect_err("oversized merged file read is rejected");

    assert!(
        matches!(error, LayerStackError::FileTooLarge { size: 6, limit: 2 }),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn commit_to_workspace_projects_active_manifest_and_rebuilds_base() -> TestResult {
    let fixture = Fixture::new("commit_workspace");
    std::fs::create_dir_all(fixture.workspace.join(".git"))?;
    std::fs::write(fixture.workspace.join(".git/config"), "[core]\n")?;
    std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;
    build_workspace_base(&fixture.root, &fixture.workspace, false)?;

    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "tracked.txt", "overlay\n")?;
    publish_text(&mut stack, "new.txt", "new\n")?;

    let (manifest, timings) = stack.commit_to_workspace(&fixture.workspace)?;

    assert_eq!(manifest.version, 1);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("tracked.txt"))?,
        "overlay\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("new.txt"))?,
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join(".git/config"))?,
        "[core]\n"
    );
    assert!(timings.contains_key("layer_stack.commit_to_workspace.project_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.replace_workspace_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.rebuild_base_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.total_s"));
    assert_eq!(stack.read_text("tracked.txt")?.0, "overlay\n");
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_too_new_manifest_schema() -> TestResult {
    let fixture = Fixture::new("workspace_base_new_schema");
    write_bound_manifest(
        &fixture,
        json!({
            "schema_version": MANIFEST_SCHEMA_VERSION + 1,
            "version": 1,
            "layers": [{"layer_id": "L000001", "path": "layers/L000001"}],
        }),
    )?;

    let Err(err) = ensure_workspace_base(&fixture.root, &fixture.workspace) else {
        return Err("too-new manifest schema was accepted".into());
    };
    assert!(
        err.to_string().contains("schema_version"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_invalid_manifest_layer_paths() -> TestResult {
    let cases = [
        ("workspace_base_empty_layer_path", ""),
        ("workspace_base_parent_layer_path", "../outside"),
        ("workspace_base_absolute_layer_path", "/abs/layer"),
        ("workspace_base_nul_layer_path", "layers/\0bad"),
    ];
    for (label, path) in cases {
        let fixture = Fixture::new(label);
        write_bound_manifest(
            &fixture,
            json!({
                "schema_version": MANIFEST_SCHEMA_VERSION,
                "version": 1,
                "layers": [{"layer_id": "L000001", "path": path}],
            }),
        )?;

        let Err(err) = ensure_workspace_base(&fixture.root, &fixture.workspace) else {
            return Err(format!("{label} was accepted").into());
        };
        assert!(
            err.to_string().contains("layer path"),
            "{label} returned unexpected error: {err}"
        );
    }
    Ok(())
}

#[test]
fn build_workspace_base_writes_manifest_with_canonical_atomic_path() -> TestResult {
    let fixture = Fixture::new("workspace_base_manifest_atomic");
    std::fs::create_dir_all(&fixture.workspace)?;
    std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;

    build_workspace_base(&fixture.root, &fixture.workspace, false)?;

    let manifest = fixture.root.join(ACTIVE_MANIFEST_FILE);
    assert!(manifest.exists());
    let manifest_payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest)?)?;
    assert_eq!(
        manifest_payload["schema_version"].as_i64(),
        Some(MANIFEST_SCHEMA_VERSION)
    );
    let stale_tmp = std::fs::read_dir(&fixture.root)?.try_fold(false, |found, entry| {
        let entry = entry?;
        Ok::<_, std::io::Error>(
            found
                || entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".manifest.json."),
        )
    })?;
    assert!(!stale_tmp, "atomic manifest writer left a temporary file");
    Ok(())
}

fn publish_text(stack: &mut LayerStack, path: &str, content: &str) -> TestResult {
    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse(path)?,
        content: content.as_bytes().to_vec(),
    }])?;
    Ok(())
}

fn publish_blob(stack: &mut LayerStack, path: &str, size: usize, seed: u8) -> TestResult {
    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse(path)?,
        content: vec![seed; size],
    }])?;
    Ok(())
}

fn payload_bytes(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    if !path.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            total += payload_bytes(&entry.path())?;
        } else if meta.is_file() || meta.file_type().is_symlink() {
            total += meta.len();
        }
    }
    Ok(total)
}

fn write_bound_manifest(fixture: &Fixture, manifest: serde_json::Value) -> TestResult {
    std::fs::create_dir_all(&fixture.root)?;
    std::fs::create_dir_all(&fixture.workspace)?;
    let binding = WorkspaceBinding {
        workspace_root: fixture.workspace.to_string_lossy().into_owned(),
        layer_stack_root: fixture.root.to_string_lossy().into_owned(),
        active_manifest_version: 1,
        active_root_hash: "root".to_owned(),
        base_manifest_version: 1,
        base_root_hash: "root".to_owned(),
    };
    std::fs::write(
        fixture.root.join(WORKSPACE_BINDING_FILE),
        serde_json::to_vec_pretty(&binding)?,
    )?;
    std::fs::write(
        fixture.root.join(ACTIVE_MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "layerstack-{label}-{}-{}",
            std::process::id(),
            NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.with_extension("workspace");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&workspace);
        Self { root, workspace }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

static NEXT_TMP_WRITE: AtomicU64 = AtomicU64::new(0);
