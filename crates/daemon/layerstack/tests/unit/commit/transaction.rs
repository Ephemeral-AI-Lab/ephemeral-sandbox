use std::path::PathBuf;

use crate::commit::route::{hash_current, PublishDecision, Route};
use crate::commit::worker::{CommitTransaction, PreparedChangeset};
use crate::commit::CommitStatus;
use crate::model::LayerChange;
use crate::stack::squash::run_auto_squash;
use crate::test_fixture::{lp, Fixture, TestResult};
use crate::{CommitOptions, LayerPath, LayerStack, LayerStackError, Manifest, MergedView};

fn transaction(fixture: &Fixture) -> CommitTransaction {
    CommitTransaction {
        root: fixture.root.clone(),
        options: CommitOptions::default(),
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        }
    }
}

fn readme_base_hash() -> TestResult<String> {
    hash_current(Some(b"# README\n"), true).ok_or_else(|| "missing readme hash".into())
}

fn base_hashes_for_snapshot(
    root: &std::path::Path,
    manifest: &Manifest,
    changes: &[LayerChange],
) -> Result<Vec<(LayerPath, Option<String>)>, LayerStackError> {
    let view = MergedView::new(root.to_path_buf());
    changes
        .iter()
        .map(|change| {
            if matches!(change, LayerChange::OpaqueDir { .. }) {
                return Ok((change.path().clone(), None));
            }
            let (bytes, exists) = view.read_bytes(change.path().as_str(), manifest)?;
            Ok((
                change.path().clone(),
                hash_current(bytes.as_deref(), exists),
            ))
        })
        .collect()
}

fn publish_decision(
    path: &str,
    route: Route,
    base_hash: Option<String>,
) -> TestResult<PublishDecision> {
    let path = lp(path)?;
    Ok(match route {
        Route::Gated => PublishDecision::gated(path, base_hash),
        Route::Direct => PublishDecision::direct(path),
        Route::Drop => PublishDecision::dropped(path, None),
    })
}

#[test]
fn publish_failpoint_marker_is_inert_without_test_opt_in() -> TestResult {
    let _guard = EnvVarGuard::unset("EOS_LAYERSTACK_ENABLE_TEST_FAILPOINTS");
    let fixture = Fixture::new("publish_failpoint_inert")?;
    let marker = fixture
        .root
        .join(".layer-metadata")
        .join("fail-next-publish");
    std::fs::create_dir_all(marker.parent().expect("marker parent"))?;
    std::fs::write(&marker, b"fail\n")?;

    let manifest =
        LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
            path: lp("new.txt")?,
            content: b"new\n".to_vec(),
        }])?;

    assert_eq!(manifest.version, 2);
    assert!(
        marker.exists(),
        "disabled failpoint must not consume marker"
    );
    Ok(())
}

#[test]
fn base_hashes_accept_opaque_dir_over_existing_directory() -> TestResult {
    let fixture = Fixture::new("opaque_base_hash")?;
    std::fs::create_dir_all(fixture.root.join("layers/B000001-base/opaque_dir"))?;
    std::fs::write(
        fixture.root.join("layers/B000001-base/opaque_dir/old.txt"),
        "old\n",
    )?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;

    let hashes = base_hashes_for_snapshot(
        &fixture.root,
        &manifest,
        &[LayerChange::OpaqueDir {
            path: lp("opaque_dir")?,
        }],
    )?;

    assert_eq!(hashes, vec![(lp("opaque_dir")?, None)]);
    Ok(())
}

#[test]
fn gated_stale_base_aborts_without_publish() -> TestResult {
    let fixture = Fixture::new("gated_stale")?;
    let old_hash = readme_base_hash()?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("README.md")?,
        content: b"# theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            decisions: vec![publish_decision("README.md", Route::Gated, Some(old_hash))?],
            changes: vec![LayerChange::Write {
                path: lp("README.md")?,
                content: b"# mine\n".to_vec(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::AbortedVersion);
    assert_eq!(
        result.files[0].observed_state.as_deref(),
        Some("content_changed")
    );
    let events = result.trace_events();
    assert_eq!(events.len(), 4);
    assert_eq!(events[2].module, "occ");
    assert_eq!(events[2].name, "commit_finished");
    assert_eq!(events[2].details["success"], false);
    assert_eq!(events[2].details["aborted_version_file_count"], 1);
    assert_eq!(events[3].module, "occ");
    assert_eq!(events[3].name, "conflict_detected");
    assert_eq!(events[3].details["path"], "README.md");
    assert_eq!(events[3].details["reason"], "aborted_version");
    assert_eq!(events[3].details["message"], "content changed");
    assert_eq!(events[3].details["observed_state"], "content_changed");
    assert_eq!(fixture.read_text("README.md")?, "# theirs\n");
    Ok(())
}

#[test]
fn direct_route_ignores_stale_base_and_publishes() -> TestResult {
    let fixture = Fixture::new("direct_stale")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("target/out.txt")?,
        content: b"theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            decisions: vec![publish_decision(
                "target/out.txt",
                Route::Direct,
                Some("stale".to_owned()),
            )?],
            changes: vec![LayerChange::Write {
                path: lp("target/out.txt")?,
                content: b"mine\n".to_vec(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert!(result.success());
    assert_eq!(result.files[0].status, CommitStatus::Committed);
    let events = result.trace_events();
    let manifest = events
        .iter()
        .find(|event| event.module == "layer_stack" && event.name == "manifest_validated")
        .expect("manifest validation event");
    assert_eq!(manifest.details["manifest_version"], 2);
    assert_eq!(manifest.details["manifest_depth"], 2);
    assert_eq!(manifest.details["manifest_path_count"], 2);
    assert_eq!(manifest.details["active_lease_count"], 0);
    let published = events
        .iter()
        .find(|event| event.module == "layer_stack" && event.name == "publish_layer_finished")
        .expect("publish layer event");
    assert_eq!(published.details["success"], true);
    assert_eq!(published.details["manifest_version_before"], 2);
    assert_eq!(published.details["manifest_version_after"], 3);
    assert_eq!(published.details["published_manifest_version"], 3);
    assert_eq!(published.details["published_layer_count"], 1);
    assert_eq!(fixture.read_text("target/out.txt")?, "mine\n");
    Ok(())
}

#[test]
fn auto_squash_skip_reason_is_traced_when_stack_is_too_shallow() -> TestResult {
    let fixture = Fixture::new("auto_squash_too_shallow")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;

    let trace = run_auto_squash(&mut stack, crate::AUTO_SQUASH_MAX_DEPTH);

    assert!(trace.timings.is_empty());
    assert_eq!(trace.events.len(), 1);
    assert_eq!(trace.events[0].module, "layer_stack");
    assert_eq!(trace.events[0].name, "auto_squash_skipped");
    assert_eq!(trace.events[0].details["reason"], "too_shallow");
    assert_eq!(trace.events[0].details["max_depth"], 100);
    assert_eq!(trace.events[0].details["depth_before"], 1);
    Ok(())
}

#[test]
fn auto_squash_finished_event_records_depth_and_manifest() -> TestResult {
    let fixture = Fixture::new("auto_squash_finished")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 0..3 {
        stack.publish_layer(&[LayerChange::Write {
            path: lp(&format!("file-{index}.txt"))?,
            content: format!("{index}\n").into_bytes(),
        }])?;
    }

    let trace = run_auto_squash(&mut stack, 2);

    assert_eq!(trace.events.len(), 2);
    assert_eq!(trace.events[0].module, "layer_stack");
    assert_eq!(trace.events[0].name, "auto_squash_started");
    assert_eq!(trace.events[0].details["max_depth"], 2);
    assert_eq!(trace.events[0].details["depth_before"], 4);
    assert_eq!(trace.events[1].module, "layer_stack");
    assert_eq!(trace.events[1].name, "auto_squash_finished");
    assert_eq!(trace.events[1].details["success"], true);
    assert_eq!(trace.events[1].details["max_depth"], 2);
    assert_eq!(trace.events[1].details["depth_before"], 4);
    assert_eq!(trace.events[1].details["depth_after"], 1);
    assert_eq!(trace.events[1].details["manifest_version"], 5);
    assert!(trace
        .timings
        .contains_key("layer_stack.auto_squash.total_s"));
    Ok(())
}

#[test]
fn auto_squash_preserves_head_visible_ignored_heavy_layers() -> TestResult {
    let fixture = Fixture::new("auto_squash_ignored_heavy")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 0..6 {
        stack.publish_layer(&[
            LayerChange::Write {
                path: lp(&format!("target/cache-{}.bin", index % 2))?,
                content: format!("cache-{index}\n").into_bytes(),
            },
            LayerChange::Write {
                path: lp("target/shared.bin")?,
                content: format!("shared-{index}\n").into_bytes(),
            },
        ])?;
    }
    let depth_before = stack.read_active_manifest()?.layers.len();

    let trace = run_auto_squash(&mut stack, 2);

    let depth_after = stack.read_active_manifest()?.layers.len();
    assert!(depth_after < depth_before);
    assert_eq!(fixture.read_text("target/cache-0.bin")?, "cache-4\n");
    assert_eq!(fixture.read_text("target/cache-1.bin")?, "cache-5\n");
    assert_eq!(fixture.read_text("target/shared.bin")?, "shared-5\n");
    let finished = trace
        .events
        .iter()
        .find(|event| event.module == "layer_stack" && event.name == "auto_squash_finished")
        .expect("auto-squash finished event");
    assert_eq!(finished.details["success"], true);
    Ok(())
}

#[test]
fn auto_squash_failure_finishes_with_error_reason() -> TestResult {
    let fixture = Fixture::new("auto_squash_failed")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    for index in 0..3 {
        stack.publish_layer(&[LayerChange::Write {
            path: lp(&format!("file-{index}.txt"))?,
            content: format!("{index}\n").into_bytes(),
        }])?;
    }
    let manifest = stack.read_active_manifest()?;
    let missing_layer = fixture.root.join(&manifest.layers[1].path);
    std::fs::remove_dir_all(missing_layer)?;

    let trace = run_auto_squash(&mut stack, 2);

    assert_eq!(trace.events.len(), 2);
    assert_eq!(trace.events[0].module, "layer_stack");
    assert_eq!(trace.events[0].name, "auto_squash_started");
    assert_eq!(trace.events[0].details["max_depth"], 2);
    assert_eq!(trace.events[0].details["depth_before"], 4);
    assert_eq!(trace.events[1].module, "layer_stack");
    assert_eq!(trace.events[1].name, "auto_squash_finished");
    assert_eq!(trace.events[1].details["success"], false);
    assert_eq!(trace.events[1].details["max_depth"], 2);
    assert_eq!(trace.events[1].details["depth_before"], 4);
    assert!(trace.events[1].details["error"].is_string());
    assert!(trace
        .timings
        .contains_key("layer_stack.auto_squash.total_s"));
    Ok(())
}

#[test]
fn gated_symlink_change_validates_and_publishes() -> TestResult {
    let fixture = Fixture::new("gated_symlink")?;
    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            decisions: vec![publish_decision("link.txt", Route::Gated, None)?],
            changes: vec![LayerChange::Symlink {
                path: lp("link.txt")?,
                source_path: "target.txt".to_owned(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert!(result.success());
    assert_eq!(result.files[0].status, CommitStatus::Committed);
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let projected = fixture.base.join("projected");
    crate::MergedView::new(fixture.root.clone()).project(&projected, &manifest)?;
    assert_eq!(
        std::fs::read_link(projected.join("link.txt"))?,
        PathBuf::from("target.txt")
    );
    Ok(())
}

#[test]
fn atomic_mixed_validation_failure_drops_accepted_paths() -> TestResult {
    let fixture = Fixture::new("atomic_mixed")?;
    let old_hash = readme_base_hash()?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("README.md")?,
        content: b"# theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            decisions: vec![
                publish_decision("README.md", Route::Gated, Some(old_hash))?,
                publish_decision("target/out.txt", Route::Direct, None)?,
            ],
            changes: vec![
                LayerChange::Write {
                    path: lp("README.md")?,
                    content: b"# mine\n".to_vec(),
                },
                LayerChange::Write {
                    path: lp("target/out.txt")?,
                    content: b"ok\n".to_vec(),
                },
            ],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::AbortedVersion);
    assert_eq!(result.files[1].status, CommitStatus::Dropped);
    assert_eq!(fixture.read_text("README.md")?, "# theirs\n");
    assert!(
        !LayerStack::open(fixture.root.clone())?
            .read_bytes("target/out.txt")?
            .1
    );
    Ok(())
}
