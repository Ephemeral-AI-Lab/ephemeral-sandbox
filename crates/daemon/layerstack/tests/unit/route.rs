use crate::model::LayerChange;
use std::path::Path;
use std::time::Duration;

use crate::test_fixture::{lp, Fixture, TestResult};
use crate::{service, CommitOptions, CommitStatus, LayerStack};
use crate::{ProtectedPathDrop, ProtectedPathDropReason};

use super::{
    capture_route_stats_for_manifest_with_protected_drops,
    publish_command_decisions_for_manifest_with_protected_drops, publish_decision_for_opaque_dir,
    publish_decisions_for_manifest_with_protected_drops, route_for_path, GitMetadataPolicy,
    ManifestIgnoreSource, Route, COMMAND_SCRATCH_PATH_DROP_REASON, DAEMON_CONTROL_PATH_DROP_REASON,
    GIT_HOOK_WRITE_REJECT_REASON, GIT_INCOMPLETE_OPERATION_REJECT_REASON,
    GIT_INDEX_STAGED_STATE_REJECT_REASON, GIT_INDEX_STAT_REFRESH_DROP_REASON,
    GIT_LOCK_FILE_REJECT_REASON, GIT_METADATA_DELETE_REJECT_REASON,
    GIT_METADATA_OPAQUE_REPLACE_REJECT_REASON, GIT_METADATA_UNSUPPORTED_DROP_REASON,
    GIT_OBJECT_REWRITE_REJECT_REASON, GIT_REFLOG_REWRITE_REJECT_REASON,
    GIT_REF_WRITE_REJECT_REASON, INVALID_LAYER_PATH_DROP_REASON,
    OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON, OPAQUE_DIR_MIXED_ROUTES_DROP_REASON,
    OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON, UNSUPPORTED_SPECIAL_FILE_DROP_REASON,
};

#[test]
fn spool_backed_write_publishes_layer_content() -> TestResult {
    let fixture = Fixture::new("spool_backed_write")?;
    let payload = fixture.base.join("payload.bin");
    std::fs::write(&payload, b"spooled ignored payload")?;

    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::WriteFile {
        path: lp("cache/payload.bin")?,
        source_path: payload,
        size: 23,
    }])?;

    assert_eq!(
        fixture.read_text("cache/payload.bin")?,
        "spooled ignored payload"
    );
    Ok(())
}

#[test]
fn bounded_capture_drops_oversized_ignored_before_payload_read_and_keeps_source() -> TestResult {
    let fixture = Fixture::new_with_gitignore("bounded_capture_drop", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-capture-drop")?;
    let upperdir = fixture.base.join("upper-bounded-drop");
    std::fs::create_dir_all(upperdir.join("ignored"))?;
    std::fs::write(upperdir.join("source.txt"), b"source")?;
    let ignored = upperdir.join("ignored/huge.bin");
    std::fs::File::create(&ignored)?.set_len((8 * 1024 * 1024) + 1)?;
    let spool_dir = fixture.base.join("spool-drop");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions {
            ignored_limits: service::IgnoredCaptureLimits {
                max_ignored_file_bytes: 1024,
                ..service::IgnoredCaptureLimits::default()
            },
            ..service::BoundedCaptureOptions::default()
        },
    )?;

    assert_eq!(captured.route_stats.gated_path_count, 1);
    assert_eq!(captured.route_stats.direct_path_count, 1);
    assert_eq!(captured.route_stats.direct_bytes, (8 * 1024 * 1024) + 1);
    assert_eq!(
        captured.route_stats.ignored_limit_drop_reason.as_deref(),
        Some(service::IGNORED_FILE_BYTE_LIMIT_DROP_REASON)
    );
    assert!(captured
        .changes
        .iter()
        .any(|change| change.path().as_str() == "source.txt"));
    assert!(captured
        .changes
        .iter()
        .all(|change| change.path().as_str() != "ignored/huge.bin"));
    assert!(
        !spool_dir.exists(),
        "limit-dropped ignored payloads are not spooled"
    );

    service::publish_capture_with_options_and_protected_drops(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
        &captured.protected_drops,
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(fixture.read_text("source.txt")?, "source");
    assert!(
        !LayerStack::open(fixture.root.clone())?
            .read_bytes("ignored/huge.bin")?
            .1
    );
    Ok(())
}

#[test]
fn bounded_capture_spools_accepted_ignored_payloads() -> TestResult {
    let fixture = Fixture::new_with_gitignore("bounded_capture_spool", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-capture-spool")?;
    let upperdir = fixture.base.join("upper-bounded-spool");
    std::fs::create_dir_all(upperdir.join("ignored"))?;
    std::fs::write(upperdir.join("ignored/payload.bin"), b"0123456789abcdef")?;
    let spool_dir = fixture.base.join("spool-accepted");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions {
            ignored_limits: service::IgnoredCaptureLimits {
                spool_threshold_bytes: 4,
                max_metadata_capture_duration: Duration::from_secs(30),
                ..service::IgnoredCaptureLimits::default()
            },
            ..service::BoundedCaptureOptions::default()
        },
    )?;

    assert_eq!(captured.route_stats.direct_path_count, 1);
    assert_eq!(captured.route_stats.direct_bytes, 16);
    assert_eq!(captured.route_stats.direct_spooled_bytes, 16);
    assert!(matches!(
        captured.changes.as_slice(),
        [LayerChange::WriteFile { size: 16, .. }]
    ));
    assert!(
        spool_dir.exists(),
        "accepted large ignored payload should be spooled"
    );

    service::publish_capture_with_options_and_protected_drops(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
        &captured.protected_drops,
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(
        fixture.read_text("ignored/payload.bin")?,
        "0123456789abcdef"
    );
    std::fs::remove_dir_all(&spool_dir)?;
    assert!(!spool_dir.exists());
    Ok(())
}

#[test]
fn bounded_capture_spools_multiple_accepted_ignored_payloads_by_aggregate_size() -> TestResult {
    let fixture = Fixture::new_with_gitignore("bounded_capture_aggregate_spool", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-capture-aggregate-spool")?;
    let upperdir = fixture.base.join("upper-bounded-aggregate-spool");
    write_sparse_file(&upperdir.join("ignored/a.bin"), 6)?;
    write_sparse_file(&upperdir.join("ignored/b.bin"), 6)?;
    let spool_dir = fixture.base.join("spool-aggregate");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions {
            ignored_limits: service::IgnoredCaptureLimits {
                spool_threshold_bytes: 10,
                max_metadata_capture_duration: Duration::from_secs(30),
                ..service::IgnoredCaptureLimits::default()
            },
            ..service::BoundedCaptureOptions::default()
        },
    )?;

    assert_eq!(captured.route_stats.direct_path_count, 2);
    assert_eq!(captured.route_stats.direct_bytes, 12);
    assert_eq!(captured.route_stats.direct_spooled_bytes, 12);
    assert_eq!(
        captured
            .changes
            .iter()
            .filter(|change| matches!(change, LayerChange::WriteFile { .. }))
            .count(),
        2
    );
    assert!(
        spool_dir.exists(),
        "aggregate-spooled ignored payloads should be stored in command spool"
    );

    service::publish_capture_with_options_and_protected_drops(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
        &captured.protected_drops,
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(
        LayerStack::open(fixture.root.clone())?
            .read_bytes("ignored/a.bin")?
            .0
            .expect("a.bin")
            .len(),
        6
    );
    assert_eq!(
        LayerStack::open(fixture.root.clone())?
            .read_bytes("ignored/b.bin")?
            .0
            .expect("b.bin")
            .len(),
        6
    );
    std::fs::remove_dir_all(&spool_dir)?;
    assert!(!spool_dir.exists());
    Ok(())
}

#[test]
fn bounded_capture_spools_nested_snapshot_ignored_payloads() -> TestResult {
    let fixture = Fixture::new("bounded_capture_nested_spool")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("pkg/.gitignore")?,
        content: b"ignored/\n".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-capture-nested-spool")?;
    let upperdir = fixture.base.join("upper-bounded-nested-spool");
    write_sparse_file(&upperdir.join("pkg/ignored/large.bin"), (1024 * 1024) + 1)?;
    let spool_dir = fixture.base.join("spool-nested");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(captured.route_stats.direct_path_count, 1);
    assert_eq!(captured.route_stats.direct_bytes, (1024 * 1024) + 1);
    assert_eq!(captured.route_stats.direct_spooled_bytes, (1024 * 1024) + 1);
    assert!(matches!(
        captured.changes.as_slice(),
        [LayerChange::WriteFile { size, .. }] if *size == (1024 * 1024) + 1
    ));
    std::fs::remove_dir_all(&spool_dir)?;
    Ok(())
}

#[test]
fn bounded_capture_reports_non_file_size_limit_reasons_before_payload_reads() -> TestResult {
    for case in [
        LimitCase {
            label: "file-count",
            file_sizes: &[1, 1],
            limits: service::IgnoredCaptureLimits {
                max_ignored_files: 1,
                max_metadata_capture_duration: Duration::from_secs(30),
                ..service::IgnoredCaptureLimits::default()
            },
            expected_reason: service::IGNORED_LANE_FILE_LIMIT_DROP_REASON,
        },
        LimitCase {
            label: "aggregate-bytes",
            file_sizes: &[6, 6],
            limits: service::IgnoredCaptureLimits {
                max_ignored_bytes: 10,
                max_metadata_capture_duration: Duration::from_secs(30),
                ..service::IgnoredCaptureLimits::default()
            },
            expected_reason: service::IGNORED_LANE_BYTE_LIMIT_DROP_REASON,
        },
        LimitCase {
            label: "metadata-duration",
            file_sizes: &[1, 1, 1, 1],
            limits: service::IgnoredCaptureLimits {
                max_metadata_capture_duration: Duration::ZERO,
                ..service::IgnoredCaptureLimits::default()
            },
            expected_reason: service::IGNORED_CAPTURE_DURATION_LIMIT_DROP_REASON,
        },
    ] {
        let fixture = Fixture::new_with_gitignore(
            &format!("bounded_capture_limit_{}", case.label),
            "ignored/\n",
        )?;
        let snapshot = service::acquire_snapshot(&fixture.root, case.label)?;
        let upperdir = fixture.base.join(format!("upper-{}", case.label));
        for (index, size) in case.file_sizes.iter().enumerate() {
            write_sparse_file(&upperdir.join(format!("ignored/{index}.bin")), *size)?;
        }
        let spool_dir = fixture.base.join(format!("spool-{}", case.label));

        let captured = service::capture_upperdir_for_snapshot_with_options(
            &fixture.root,
            snapshot.manifest_version,
            &snapshot.layer_paths,
            &upperdir,
            &spool_dir,
            service::BoundedCaptureOptions {
                ignored_limits: case.limits,
                ..service::BoundedCaptureOptions::default()
            },
        )?;
        service::release_lease(&fixture.root, &snapshot.lease_id)?;

        assert_eq!(
            captured.route_stats.ignored_limit_drop_reason.as_deref(),
            Some(case.expected_reason),
            "limit reason for {}",
            case.label
        );
        assert!(
            captured.changes.is_empty(),
            "ignored lane should be dropped before payload materialization for {}",
            case.label
        );
        assert!(
            !spool_dir.exists(),
            "limit-dropped ignored payloads should not be spooled for {}",
            case.label
        );
    }
    Ok(())
}

struct LimitCase {
    label: &'static str,
    file_sizes: &'static [u64],
    limits: service::IgnoredCaptureLimits,
    expected_reason: &'static str,
}

fn write_sparse_file(path: &Path, len: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)?.set_len(len)
}

fn git_index_with_entry(path: &str, object_byte: u8, stat_seed: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DIRC");
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(&stat_seed.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&stat_seed.saturating_add(1).to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&stat_seed.saturating_add(2).to_be_bytes());
    bytes.extend_from_slice(&stat_seed.saturating_add(3).to_be_bytes());
    bytes.extend_from_slice(&0o100644_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&12_u32.to_be_bytes());
    bytes.extend_from_slice(&[object_byte; 20]);
    let path_len = u16::try_from(path.len()).expect("test path fits index flags");
    bytes.extend_from_slice(&path_len.to_be_bytes());
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    while bytes.len() % 8 != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&[0; 20]);
    bytes
}

fn git_empty_index() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DIRC");
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&[0; 20]);
    bytes
}

fn is_ignored(fixture: &Fixture, path: &str) -> TestResult<bool> {
    Ok(route_of(fixture, path)? == Route::Direct)
}

fn route_of(fixture: &Fixture, path: &str) -> TestResult<Route> {
    let stack = LayerStack::open(fixture.root.clone())?;
    Ok(route_for_path(&stack, &lp(path)?)?)
}

fn file_status(result: &crate::ChangesetResult, path: &str) -> TestResult<CommitStatus> {
    Ok(result
        .files
        .iter()
        .find(|file| file.path == lp(path).expect("valid layer path"))
        .ok_or_else(|| format!("missing file result for {path}"))?
        .status)
}

fn file_message(result: &crate::ChangesetResult, path: &str) -> TestResult<String> {
    Ok(result
        .files
        .iter()
        .find(|file| file.path == lp(path).expect("valid layer path"))
        .ok_or_else(|| format!("missing file result for {path}"))?
        .message
        .clone())
}

#[test]
fn root_gitignore_routes_target_as_direct() -> TestResult {
    let fixture = Fixture::new_with_gitignore("gitignore_direct", "target/\n*.pyc\n")?;

    assert!(is_ignored(&fixture, "target/out.txt")?);
    assert!(is_ignored(&fixture, "pkg/cache.pyc")?);
    assert!(!is_ignored(&fixture, "src/main.rs")?);
    Ok(())
}

#[test]
fn routes_tracked_ignored_and_git_paths_distinctly() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_kinds", "target/\n*.pyc\n")?;

    assert_eq!(route_of(&fixture, "src/main.rs")?, Route::Gated);
    assert_eq!(route_of(&fixture, "target/out.txt")?, Route::Direct);
    assert_eq!(route_of(&fixture, "pkg/cache.pyc")?, Route::Direct);
    assert_eq!(route_of(&fixture, ".git/config")?, Route::Drop);
    assert_eq!(route_of(&fixture, "pkg/.git/config")?, Route::Drop);
    Ok(())
}

#[test]
fn protected_paths_drop_before_source_or_ignore_routing() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_protected_paths", "*\n")?;

    for path in [
        "manifest.json",
        "workspace.json",
        "layers/B000001-base/README.md",
        "staging/B000002.staging/file.txt",
        ".layer-metadata/B000001-base.digest",
        "tree/.layer-metadata/state.json",
        "command-runner-request.json",
        "command-runner-result.json",
        "runner-request.json",
        "runner-result.json",
        "metadata.json",
        "final.json",
        "transcript.log",
        "spool/payload.bin",
        "commands/cmd_1/transcript.log",
        ".eos-command/cmd_1/final.json",
    ] {
        assert_eq!(route_of(&fixture, path)?, Route::Drop, "route for {path}");
    }

    assert_eq!(route_of(&fixture, "ordinary.txt")?, Route::Direct);
    Ok(())
}

#[test]
fn git_metadata_drop_decisions_use_stable_reason_code() -> TestResult {
    let fixture = Fixture::new_with_gitignore("git_drop_reason", ".git/\n")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let decisions = publish_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp(".git")?,
                content: b"gitdir".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/config")?,
                content: b"config".to_vec(),
            },
            LayerChange::Write {
                path: lp("src/main.rs")?,
                content: b"source".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(decisions[0].route, Route::Drop);
    assert_eq!(
        decisions[0].drop_reason.map(|reason| reason.as_str()),
        Some(GIT_METADATA_UNSUPPORTED_DROP_REASON)
    );
    assert_eq!(decisions[1].route, Route::Drop);
    assert_eq!(
        decisions[1].drop_reason.map(|reason| reason.as_str()),
        Some(GIT_METADATA_UNSUPPORTED_DROP_REASON)
    );
    assert_eq!(decisions[2].route, Route::Gated);
    assert_eq!(decisions[2].drop_reason, None);
    Ok(())
}

#[test]
fn command_git_index_stat_refresh_is_dropped_without_conflict_or_publish() -> TestResult {
    let fixture = Fixture::new("command_git_index_stat_refresh")?;
    let base_index = git_index_with_entry("src/main.rs", 1, 10);
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/index")?,
        content: base_index,
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-index-stat-refresh")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("src/other.rs")?,
        content: b"other".to_vec(),
    }])?;

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp(".git/index")?,
            content: git_index_with_entry("src/main.rs", 1, 99),
        }],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(result.success());
    assert_eq!(result.published_manifest_version, None);
    assert_eq!(file_status(&result, ".git/index")?, CommitStatus::Dropped);
    assert_eq!(
        file_message(&result, ".git/index")?,
        GIT_INDEX_STAT_REFRESH_DROP_REASON
    );
    Ok(())
}

#[test]
fn command_git_empty_index_creation_is_dropped_as_noop() -> TestResult {
    let fixture = Fixture::new("command_git_empty_index")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-empty-index")?;

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp(".git/index")?,
            content: git_empty_index(),
        }],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(result.success());
    assert_eq!(result.published_manifest_version, None);
    assert_eq!(file_status(&result, ".git/index")?, CommitStatus::Dropped);
    assert_eq!(
        file_message(&result, ".git/index")?,
        GIT_INDEX_STAT_REFRESH_DROP_REASON
    );
    Ok(())
}

#[test]
fn bounded_command_capture_preserves_git_index_stat_refresh_drop() -> TestResult {
    let fixture = Fixture::new("bounded_command_git_index_stat_refresh")?;
    let base_index = git_index_with_entry("src/main.rs", 1, 10);
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/index")?,
        content: base_index,
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-git-index-stat-refresh")?;
    let upperdir = fixture.base.join("upper-index-stat-refresh");
    std::fs::create_dir_all(upperdir.join(".git"))?;
    std::fs::write(
        upperdir.join(".git/index"),
        git_index_with_entry("src/main.rs", 1, 99),
    )?;
    let spool_dir = fixture.base.join("spool-index-stat-refresh");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions::default(),
    )?;
    assert_eq!(
        captured
            .route_stats
            .drop_reason_count(GIT_INDEX_STAT_REFRESH_DROP_REASON),
        1
    );

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
        &captured.protected_drops,
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(result.success());
    assert_eq!(result.published_manifest_version, None);
    assert_eq!(file_status(&result, ".git/index")?, CommitStatus::Dropped);
    assert_eq!(
        file_message(&result, ".git/index")?,
        GIT_INDEX_STAT_REFRESH_DROP_REASON
    );
    Ok(())
}

#[test]
fn bounded_command_capture_path_only_git_reject_does_not_read_payload() -> TestResult {
    let fixture = Fixture::new("bounded_command_git_hook_large_reject")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "bounded-git-hook-large-reject")?;
    let upperdir = fixture.base.join("upper-large-hook");
    write_sparse_file(
        &upperdir.join(".git/hooks/pre-commit"),
        (8 * 1024 * 1024) + 1,
    )?;
    let spool_dir = fixture.base.join("spool-large-hook");

    let captured = service::capture_upperdir_for_snapshot_with_options(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &upperdir,
        &spool_dir,
        service::BoundedCaptureOptions::default(),
    )?;
    assert_eq!(
        captured
            .route_stats
            .drop_reason_count(GIT_HOOK_WRITE_REJECT_REASON),
        1
    );

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &captured.changes,
        &captured.protected_drops,
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(!result.success());
    assert_eq!(result.published_manifest_version, None);
    assert_eq!(
        file_status(&result, ".git/hooks/pre-commit")?,
        CommitStatus::Failed
    );
    assert_eq!(
        file_message(&result, ".git/hooks/pre-commit")?,
        GIT_HOOK_WRITE_REJECT_REASON
    );
    Ok(())
}

#[test]
fn command_git_staged_index_rejects_whole_publish() -> TestResult {
    let fixture = Fixture::new_with_gitignore("command_git_staged_index", "cache/\n")?;
    let base_index = git_index_with_entry("src/main.rs", 1, 10);
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/index")?,
        content: base_index,
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-staged-index")?;

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[
            LayerChange::Write {
                path: lp(".git/index")?,
                content: git_index_with_entry("src/main.rs", 2, 10),
            },
            LayerChange::Write {
                path: lp("cache/out.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(!result.success());
    assert_eq!(result.published_manifest_version, None);
    assert_eq!(file_status(&result, ".git/index")?, CommitStatus::Failed);
    assert_eq!(
        file_message(&result, ".git/index")?,
        GIT_INDEX_STAGED_STATE_REJECT_REASON
    );
    assert_eq!(
        file_status(&result, "cache/out.txt")?,
        CommitStatus::Dropped
    );
    assert!(
        !LayerStack::open(fixture.root.clone())?
            .read_bytes("cache/out.txt")?
            .1,
        "ignored output must not publish when git metadata rejects"
    );
    Ok(())
}

#[test]
fn command_git_rejects_locks_markers_hooks_and_ref_writes() -> TestResult {
    let fixture = Fixture::new("command_git_reject_control_paths")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let decisions = publish_command_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp(".git/index.lock")?,
                content: b"lock".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/MERGE_HEAD")?,
                content: b"merge".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/rebase-merge/head-name")?,
                content: b"main".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/hooks/pre-commit")?,
                content: b"hook".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/refs/heads/main")?,
                content: b"abc".to_vec(),
            },
        ],
        &[],
    )?;

    let expected = [
        GIT_LOCK_FILE_REJECT_REASON,
        GIT_INCOMPLETE_OPERATION_REJECT_REASON,
        GIT_INCOMPLETE_OPERATION_REJECT_REASON,
        GIT_HOOK_WRITE_REJECT_REASON,
        GIT_REF_WRITE_REJECT_REASON,
    ];
    for (decision, reason) in decisions.iter().zip(expected) {
        assert_eq!(decision.route, Route::Drop);
        assert!(decision.reject_publish);
        assert_eq!(
            decision.drop_reason.map(|reason| reason.as_str()),
            Some(reason)
        );
    }
    Ok(())
}

#[test]
fn command_git_deletions_and_opaque_root_reject() -> TestResult {
    let fixture = Fixture::new("command_git_delete_reject")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let decisions = publish_command_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Delete {
                path: lp(".git/HEAD")?,
            },
            LayerChange::Delete {
                path: lp(".git/objects/aa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?,
            },
            LayerChange::Delete {
                path: lp(".git/refs/heads/main")?,
            },
            LayerChange::Delete {
                path: lp(".git/index")?,
            },
            LayerChange::Delete {
                path: lp(".git/config")?,
            },
            LayerChange::OpaqueDir { path: lp(".git")? },
        ],
        &[],
    )?;

    for decision in decisions.iter().take(5) {
        assert_eq!(decision.route, Route::Drop);
        assert!(decision.reject_publish);
        assert_eq!(
            decision.drop_reason.map(|reason| reason.as_str()),
            Some(GIT_METADATA_DELETE_REJECT_REASON)
        );
    }
    assert_eq!(
        decisions[5].drop_reason.map(|reason| reason.as_str()),
        Some(GIT_METADATA_OPAQUE_REPLACE_REJECT_REASON)
    );
    assert!(decisions[5].reject_publish);
    Ok(())
}

#[test]
fn command_git_reflog_append_is_gated_and_rewrite_rejects() -> TestResult {
    let fixture = Fixture::new("command_git_reflog_append")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/logs/HEAD")?,
        content: b"old\n".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-reflog-append")?;

    let append = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp(".git/logs/HEAD")?,
            content: b"old\nnew\n".to_vec(),
        }],
        &[],
        CommitOptions::default(),
    )?;
    assert!(append.success());
    assert_eq!(
        file_status(&append, ".git/logs/HEAD")?,
        CommitStatus::Committed
    );
    assert_eq!(fixture.read_text(".git/logs/HEAD")?, "old\nnew\n");

    let rewrite = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp(".git/logs/HEAD")?,
            content: b"rewrite\n".to_vec(),
        }],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(!rewrite.success());
    assert_eq!(rewrite.published_manifest_version, None);
    assert_eq!(
        file_status(&rewrite, ".git/logs/HEAD")?,
        CommitStatus::Failed
    );
    assert_eq!(
        file_message(&rewrite, ".git/logs/HEAD")?,
        GIT_REFLOG_REWRITE_REJECT_REASON
    );
    Ok(())
}

#[test]
fn command_git_objects_are_gated_and_rewrites_reject() -> TestResult {
    let fixture = Fixture::new("command_git_object_rules")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/objects/aa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?,
        content: b"object".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-object-rules")?;

    let decisions = publish_command_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &LayerStack::open(fixture.root.clone())?.read_active_manifest()?,
        &[
            LayerChange::Write {
                path: lp(".git/objects/cc/dddddddddddddddddddddddddddddddddddddddd")?,
                content: b"new-object".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/objects/aa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?,
                content: b"different".to_vec(),
            },
        ],
        &[],
    )?;
    assert_eq!(decisions[0].route, Route::Gated);
    assert!(!decisions[0].reject_publish);
    assert_eq!(decisions[1].route, Route::Drop);
    assert!(decisions[1].reject_publish);
    assert_eq!(
        decisions[1].drop_reason.map(|reason| reason.as_str()),
        Some(GIT_OBJECT_REWRITE_REJECT_REASON)
    );
    service::release_lease(&fixture.root, &snapshot.lease_id)?;
    Ok(())
}

#[test]
fn command_gitignore_cannot_route_git_metadata_direct_or_source() -> TestResult {
    let fixture = Fixture::new_with_gitignore("command_git_ignore_bypass", ".git/\n*\n")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp(".git/logs/HEAD")?,
        content: b"old\n".to_vec(),
    }])?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;

    let decisions = publish_command_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp(".git/logs/HEAD")?,
                content: b"old\nnew\n".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/config")?,
                content: b"config".to_vec(),
            },
            LayerChange::Write {
                path: lp("ordinary.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(decisions[0].route, Route::Gated);
    assert_eq!(decisions[1].route, Route::Drop);
    assert!(decisions[1].reject_publish);
    assert_eq!(decisions[2].route, Route::Direct);
    Ok(())
}

#[test]
fn protected_path_drop_decisions_use_stable_reason_codes() -> TestResult {
    let fixture = Fixture::new_with_gitignore("protected_drop_reasons", "*\n")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let decisions = publish_decisions_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp("manifest.json")?,
                content: b"manifest".to_vec(),
            },
            LayerChange::Write {
                path: lp(".layer-metadata/digest")?,
                content: b"digest".to_vec(),
            },
            LayerChange::Write {
                path: lp("transcript.log")?,
                content: b"transcript".to_vec(),
            },
            LayerChange::Write {
                path: lp("ordinary.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(decisions[0].route, Route::Drop);
    assert_eq!(
        decisions[0].drop_reason.map(|reason| reason.as_str()),
        Some(DAEMON_CONTROL_PATH_DROP_REASON)
    );
    assert_eq!(decisions[1].route, Route::Drop);
    assert_eq!(
        decisions[1].drop_reason.map(|reason| reason.as_str()),
        Some(DAEMON_CONTROL_PATH_DROP_REASON)
    );
    assert_eq!(decisions[2].route, Route::Drop);
    assert_eq!(
        decisions[2].drop_reason.map(|reason| reason.as_str()),
        Some(COMMAND_SCRATCH_PATH_DROP_REASON)
    );
    assert_eq!(decisions[3].route, Route::Direct);
    assert_eq!(decisions[3].drop_reason, None);
    Ok(())
}

#[test]
fn capture_route_stats_use_supplied_manifest_snapshot() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_stats_snapshot", "ignored/\n")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    let route_manifest = stack.read_active_manifest()?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp(".gitignore")?,
        content: b"later/\n".to_vec(),
    }])?;

    let stats = capture_route_stats_for_manifest_with_protected_drops(
        &fixture.root,
        &route_manifest,
        &[
            LayerChange::Write {
                path: lp("src/main.rs")?,
                content: b"source".to_vec(),
            },
            LayerChange::Write {
                path: lp("ignored/cache.txt")?,
                content: b"ignored".to_vec(),
            },
            LayerChange::Write {
                path: lp("later/cache.txt")?,
                content: b"later".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/config")?,
                content: b"git".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(stats.gated_path_count, 2);
    assert_eq!(stats.direct_path_count, 1);
    assert_eq!(stats.drop_path_count, 1);
    assert_eq!(stats.direct_bytes, 7);
    assert_eq!(
        stats.drop_reason_count(GIT_METADATA_UNSUPPORTED_DROP_REASON),
        1
    );
    Ok(())
}

#[test]
fn capture_route_stats_counts_git_drop_reasons() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_stats_git_drop_reasons", ".git/\n")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let stats = capture_route_stats_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp(".git")?,
                content: b"gitdir".to_vec(),
            },
            LayerChange::Write {
                path: lp("pkg/.git/hooks/pre-commit")?,
                content: b"hook".to_vec(),
            },
            LayerChange::Write {
                path: lp("ordinary.txt")?,
                content: b"source".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(stats.gated_path_count, 1);
    assert_eq!(stats.direct_path_count, 0);
    assert_eq!(stats.drop_path_count, 2);
    assert_eq!(
        stats.drop_reason_count(GIT_METADATA_UNSUPPORTED_DROP_REASON),
        2
    );
    Ok(())
}

#[test]
fn capture_route_stats_counts_protected_drop_reasons() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_stats_protected_drop_reasons", "target/\n")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let stats = capture_route_stats_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[LayerChange::Write {
            path: lp("target/out.txt")?,
            content: b"ignored".to_vec(),
        }],
        &[
            ProtectedPathDrop {
                path: lp("run.sock")?,
                reason: ProtectedPathDropReason::UnsupportedSpecialFile,
            },
            ProtectedPathDrop {
                path: lp(".invalid-layer-path/626164")?,
                reason: ProtectedPathDropReason::InvalidLayerPath,
            },
        ],
    )?;

    assert_eq!(stats.gated_path_count, 0);
    assert_eq!(stats.direct_path_count, 1);
    assert_eq!(stats.drop_path_count, 2);
    assert_eq!(stats.direct_bytes, 7);
    assert_eq!(
        stats.drop_reason_count(UNSUPPORTED_SPECIAL_FILE_DROP_REASON),
        1
    );
    assert_eq!(stats.drop_reason_count(INVALID_LAYER_PATH_DROP_REASON), 1);
    Ok(())
}

#[test]
fn capture_route_stats_counts_route_protected_drop_reasons() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_stats_route_protected", "*\n")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let stats = capture_route_stats_for_manifest_with_protected_drops(
        &fixture.root,
        &manifest,
        &[
            LayerChange::Write {
                path: lp("manifest.json")?,
                content: b"manifest".to_vec(),
            },
            LayerChange::Write {
                path: lp("transcript.log")?,
                content: b"transcript".to_vec(),
            },
            LayerChange::Write {
                path: lp("ordinary.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
    )?;

    assert_eq!(stats.gated_path_count, 0);
    assert_eq!(stats.direct_path_count, 1);
    assert_eq!(stats.drop_path_count, 2);
    assert_eq!(stats.drop_reason_count(DAEMON_CONTROL_PATH_DROP_REASON), 1);
    assert_eq!(stats.drop_reason_count(COMMAND_SCRATCH_PATH_DROP_REASON), 1);
    Ok(())
}

#[test]
fn publish_capture_uses_supplied_manifest_snapshot_for_routes() -> TestResult {
    let fixture = Fixture::new_with_gitignore("publish_route_snapshot", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "route-snapshot-test")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[
        LayerChange::Write {
            path: lp(".gitignore")?,
            content: b"later/\n".to_vec(),
        },
        LayerChange::Write {
            path: lp("later/cache.txt")?,
            content: b"theirs".to_vec(),
        },
    ])?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp("later/cache.txt")?,
            content: b"mine".to_vec(),
        }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::AbortedVersion);
    assert_eq!(fixture.read_text("later/cache.txt")?, "theirs");
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["gated_path_count"], 1);
    assert_eq!(handoff.details["direct_path_count"], 0);
    Ok(())
}

#[test]
fn lane_aware_publish_drops_ignored_when_source_conflicts() -> TestResult {
    let fixture = Fixture::new_with_gitignore("lane_aware_source_conflict", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "lane-aware-source-conflict")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("src/main.rs")?,
        content: b"theirs".to_vec(),
    }])?;

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[
            LayerChange::Write {
                path: lp("src/main.rs")?,
                content: b"mine".to_vec(),
            },
            LayerChange::Write {
                path: lp("ignored/cache.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(
        file_status(&result, "src/main.rs")?,
        CommitStatus::AbortedVersion
    );
    assert_eq!(
        file_status(&result, "ignored/cache.txt")?,
        CommitStatus::Dropped
    );
    assert_eq!(fixture.read_text("src/main.rs")?, "theirs");
    assert!(
        !LayerStack::open(fixture.root.clone())?
            .read_bytes("ignored/cache.txt")?
            .1,
        "ignored output must not publish after source OCC conflict"
    );
    Ok(())
}

#[test]
fn lane_aware_publish_ignored_only_uses_direct_lww() -> TestResult {
    let fixture = Fixture::new_with_gitignore("lane_aware_ignored_lww", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "lane-aware-ignored-lww")?;

    let first = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp("ignored/cache.txt")?,
            content: b"first".to_vec(),
        }],
        &[],
        CommitOptions::default(),
    )?;
    let second = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp("ignored/cache.txt")?,
            content: b"second".to_vec(),
        }],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(first.success());
    assert!(second.success());
    assert_eq!(
        first.published_manifest_version.map(|version| version + 1),
        second.published_manifest_version
    );
    assert_eq!(fixture.read_text("ignored/cache.txt")?, "second");
    Ok(())
}

#[test]
fn lane_aware_publish_source_and_ignored_success_advances_one_manifest() -> TestResult {
    let fixture = Fixture::new_with_gitignore("lane_aware_mixed_success", "ignored/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "lane-aware-mixed-success")?;
    let before = LayerStack::open(fixture.root.clone())?
        .read_active_manifest()?
        .version;

    let result = service::publish_command_capture_lane_aware(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[
            LayerChange::Write {
                path: lp("src/main.rs")?,
                content: b"source".to_vec(),
            },
            LayerChange::Write {
                path: lp("ignored/cache.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
        &[],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    let after = LayerStack::open(fixture.root.clone())?
        .read_active_manifest()?
        .version;
    assert!(result.success());
    assert_eq!(after, before + 1);
    assert_eq!(
        result.published_manifest_version,
        Some(u64::try_from(after)?)
    );
    assert_eq!(
        file_status(&result, "src/main.rs")?,
        CommitStatus::Committed
    );
    assert_eq!(
        file_status(&result, "ignored/cache.txt")?,
        CommitStatus::Committed
    );
    assert_eq!(fixture.read_text("src/main.rs")?, "source");
    assert_eq!(fixture.read_text("ignored/cache.txt")?, "ignored");
    Ok(())
}

#[test]
fn publish_capture_surfaces_git_drop_reason_counts() -> TestResult {
    let fixture = Fixture::new_with_gitignore("publish_git_drop_reason", "target/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "git-drop-reason-test")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[
            LayerChange::Write {
                path: lp(".git/config")?,
                content: b"git".to_vec(),
            },
            LayerChange::Write {
                path: lp("target/out.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    let git_result = result
        .files
        .iter()
        .find(|file| file.path == lp(".git/config").expect("valid git path"))
        .expect("git file result");
    assert_eq!(git_result.status, CommitStatus::Dropped);
    assert_eq!(git_result.message, GIT_METADATA_UNSUPPORTED_DROP_REASON);
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["drop_path_count"], 1);
    assert_eq!(
        handoff.details["drop_reason_counts"][GIT_METADATA_UNSUPPORTED_DROP_REASON],
        1
    );
    Ok(())
}

#[test]
fn publish_capture_surfaces_protected_drop_reason_counts() -> TestResult {
    let fixture = Fixture::new_with_gitignore("publish_protected_drop_reason", "target/\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "protected-drop-reason-test")?;

    let result = service::publish_capture_with_options_and_protected_drops(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::Write {
            path: lp("target/out.txt")?,
            content: b"ignored".to_vec(),
        }],
        &[
            ProtectedPathDrop {
                path: lp("run.sock")?,
                reason: ProtectedPathDropReason::UnsupportedSpecialFile,
            },
            ProtectedPathDrop {
                path: lp(".invalid-layer-path/626164")?,
                reason: ProtectedPathDropReason::InvalidLayerPath,
            },
        ],
        CommitOptions::default(),
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    let protected_result = result
        .files
        .iter()
        .find(|file| file.path == lp("run.sock").expect("valid protected path"))
        .expect("protected file result");
    assert_eq!(protected_result.status, CommitStatus::Dropped);
    assert_eq!(
        protected_result.message,
        UNSUPPORTED_SPECIAL_FILE_DROP_REASON
    );
    let invalid_result = result
        .files
        .iter()
        .find(|file| file.path == lp(".invalid-layer-path/626164").expect("valid placeholder"))
        .expect("invalid path file result");
    assert_eq!(invalid_result.status, CommitStatus::Dropped);
    assert_eq!(invalid_result.message, INVALID_LAYER_PATH_DROP_REASON);
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["drop_path_count"], 2);
    assert_eq!(
        handoff.details["drop_reason_counts"][UNSUPPORTED_SPECIAL_FILE_DROP_REASON],
        1
    );
    assert_eq!(
        handoff.details["drop_reason_counts"][INVALID_LAYER_PATH_DROP_REASON],
        1
    );
    assert_eq!(fixture.read_text("target/out.txt")?, "ignored");
    Ok(())
}

#[test]
fn publish_capture_surfaces_route_protected_drop_reason_counts() -> TestResult {
    let fixture = Fixture::new_with_gitignore("publish_route_protected_drop_reason", "*\n")?;
    let snapshot = service::acquire_snapshot(&fixture.root, "route-protected-drop-reason-test")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[
            LayerChange::Write {
                path: lp("manifest.json")?,
                content: b"manifest".to_vec(),
            },
            LayerChange::Write {
                path: lp("transcript.log")?,
                content: b"transcript".to_vec(),
            },
            LayerChange::Write {
                path: lp("ordinary.txt")?,
                content: b"ignored".to_vec(),
            },
        ],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(
        result
            .files
            .iter()
            .find(|file| file.path == lp("manifest.json").expect("valid daemon path"))
            .expect("daemon control result")
            .message,
        DAEMON_CONTROL_PATH_DROP_REASON
    );
    assert_eq!(
        result
            .files
            .iter()
            .find(|file| file.path == lp("transcript.log").expect("valid scratch path"))
            .expect("command scratch result")
            .message,
        COMMAND_SCRATCH_PATH_DROP_REASON
    );
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["drop_path_count"], 2);
    assert_eq!(
        handoff.details["drop_reason_counts"][DAEMON_CONTROL_PATH_DROP_REASON],
        1
    );
    assert_eq!(
        handoff.details["drop_reason_counts"][COMMAND_SCRATCH_PATH_DROP_REASON],
        1
    );
    assert_eq!(fixture.read_text("ordinary.txt")?, "ignored");
    Ok(())
}

#[test]
fn opaque_dir_with_all_ignored_descendants_routes_direct() -> TestResult {
    let fixture = Fixture::new_with_gitignore("opaque_all_ignored", "cache/\n")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp("cache/out.txt")?,
        content: b"old-cache".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "opaque-all-ignored")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::OpaqueDir { path: lp("cache")? }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert!(result.success());
    assert_eq!(result.files[0].status, CommitStatus::Committed);
    let (_bytes, exists) = LayerStack::open(fixture.root.clone())?.read_bytes("cache/out.txt")?;
    assert!(
        !exists,
        "ignored opaque marker should hide ignored descendants"
    );
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["direct_path_count"], 1);
    assert_eq!(handoff.details["drop_path_count"], 0);
    Ok(())
}

#[test]
fn opaque_dir_with_source_descendants_validates_hidden_paths() -> TestResult {
    let fixture = Fixture::new("opaque_source_validation")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp("src/old.txt")?,
        content: b"old-source".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "opaque-source-validation")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("src/old.txt")?,
        content: b"theirs".to_vec(),
    }])?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::OpaqueDir { path: lp("src")? }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::AbortedVersion);
    assert!(
        result.files[0].message.contains("src/old.txt"),
        "conflict should name the hidden descendant: {:?}",
        result.files[0]
    );
    assert_eq!(fixture.read_text("src/old.txt")?, "theirs");
    Ok(())
}

#[test]
fn opaque_dir_with_mixed_descendant_routes_rejects_publish() -> TestResult {
    let fixture = Fixture::new_with_gitignore("opaque_mixed_routes", "cache/\n")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[
        LayerChange::Write {
            path: lp("tree/src.txt")?,
            content: b"source".to_vec(),
        },
        LayerChange::Write {
            path: lp("tree/cache/out.txt")?,
            content: b"ignored".to_vec(),
        },
    ])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "opaque-mixed-routes")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::OpaqueDir { path: lp("tree")? }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::Failed);
    assert_eq!(result.files[0].message, OPAQUE_DIR_MIXED_ROUTES_DROP_REASON);
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(handoff.details["drop_path_count"], 1);
    assert_eq!(
        handoff.details["drop_reason_counts"][OPAQUE_DIR_MIXED_ROUTES_DROP_REASON],
        1
    );
    assert_eq!(fixture.read_text("tree/src.txt")?, "source");
    assert_eq!(fixture.read_text("tree/cache/out.txt")?, "ignored");
    Ok(())
}

#[test]
fn opaque_dir_with_git_descendant_rejects_as_protected() -> TestResult {
    let fixture = Fixture::new("opaque_protected_descendant")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp("tree/.git/config")?,
        content: b"git".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "opaque-protected-descendant")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::OpaqueDir { path: lp("tree")? }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::Failed);
    assert_eq!(
        result.files[0].message,
        OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON
    );
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(
        handoff.details["drop_reason_counts"][OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON],
        1
    );
    let (_bytes, exists) =
        LayerStack::open(fixture.root.clone())?.read_bytes("tree/.git/config")?;
    assert!(exists, "protected descendant must remain visible");
    Ok(())
}

#[test]
fn opaque_dir_with_non_git_protected_descendant_rejects_as_protected() -> TestResult {
    let fixture = Fixture::new("opaque_non_git_protected_descendant")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp("tree/.layer-metadata/state.json")?,
        content: b"daemon-state".to_vec(),
    }])?;
    let snapshot = service::acquire_snapshot(&fixture.root, "opaque-non-git-protected")?;

    let result = service::publish_capture(
        &fixture.root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        &[LayerChange::OpaqueDir { path: lp("tree")? }],
    )?;
    service::release_lease(&fixture.root, &snapshot.lease_id)?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, CommitStatus::Failed);
    assert_eq!(
        result.files[0].message,
        OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON
    );
    let handoff = result
        .events
        .iter()
        .find(|event| event.module == "occ" && event.name == "worker_handoff")
        .expect("worker handoff event");
    assert_eq!(
        handoff.details["drop_reason_counts"][OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON],
        1
    );
    let (_bytes, exists) =
        LayerStack::open(fixture.root.clone())?.read_bytes("tree/.layer-metadata/state.json")?;
    assert!(exists, "protected descendant must remain visible");
    Ok(())
}

#[test]
fn opaque_dir_expansion_limit_rejects_publish() -> TestResult {
    let fixture = Fixture::new("opaque_expansion_limit")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;
    stack.publish_layer(&[
        LayerChange::Write {
            path: lp("big/a.txt")?,
            content: b"a".to_vec(),
        },
        LayerChange::Write {
            path: lp("big/b.txt")?,
            content: b"b".to_vec(),
        },
        LayerChange::Write {
            path: lp("big/c.txt")?,
            content: b"c".to_vec(),
        },
    ])?;
    let manifest = stack.read_active_manifest()?;
    let view = crate::MergedView::new(fixture.root.clone());
    let source = ManifestIgnoreSource {
        view: &view,
        manifest: &manifest,
    };

    let decision = publish_decision_for_opaque_dir(
        &fixture.root,
        &source,
        &view,
        &manifest,
        &lp("big")?,
        2,
        GitMetadataPolicy::UnsupportedDrop,
    )?;

    assert_eq!(decision.route, Route::Drop);
    assert!(decision.reject_publish);
    assert_eq!(
        decision.drop_reason.map(|reason| reason.as_str()),
        Some(OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON)
    );
    Ok(())
}

// N2 (HIGH): a no-slash dir-only pattern is anchored at *any* depth, so a
// file under `frontend/node_modules/` routes DIRECT — the most common
// misroute the old root-anchored prefix check produced.
#[test]
fn dir_only_pattern_matches_at_any_depth() -> TestResult {
    let fixture = Fixture::new_with_gitignore("n2_dir_only", "node_modules/\n")?;
    assert!(is_ignored(&fixture, "frontend/node_modules/index.js")?);
    assert!(is_ignored(&fixture, "node_modules/index.js")?);
    assert!(!is_ignored(&fixture, "frontend/src/index.js")?);
    Ok(())
}

// N3 (HIGH, data-loss): `*` must not cross `/`. `logs/*.log` does NOT match
// `logs/sub/x.log`, so it routes GATED (base-hash validated) — not
// DIRECT-then-silently-clobber as the old `wildcard_match` allowed.
#[test]
fn star_does_not_cross_slash() -> TestResult {
    let fixture = Fixture::new_with_gitignore("n3_star_slash", "logs/*.log\n")?;
    assert!(is_ignored(&fixture, "logs/app.log")?);
    assert!(!is_ignored(&fixture, "logs/sub/x.log")?);
    Ok(())
}

// Nested `.gitignore` is scoped to its own subtree.
#[test]
fn nested_gitignore_is_scoped_to_its_subtree() -> TestResult {
    let fixture = Fixture::new_with_gitignores("nested", &[("frontend", "dist/\n")])?;
    assert!(is_ignored(&fixture, "frontend/dist/bundle.js")?);
    assert!(!is_ignored(&fixture, "dist/bundle.js")?);
    Ok(())
}

// `**` matches across path segments.
#[test]
fn double_star_matches_across_segments() -> TestResult {
    let fixture = Fixture::new_with_gitignore("double_star", "**/build/\n")?;
    assert!(is_ignored(&fixture, "a/b/build/out.o")?);
    assert!(is_ignored(&fixture, "build/out.o")?);
    assert!(!is_ignored(&fixture, "a/b/builder.rs")?);
    Ok(())
}

// `!` re-includes within a non-sealed directory.
#[test]
fn bang_re_includes_in_unsealed_dir() -> TestResult {
    let fixture = Fixture::new_with_gitignore("bang", "*.log\n!keep.log\n")?;
    assert!(is_ignored(&fixture, "other.log")?);
    assert!(!is_ignored(&fixture, "keep.log")?);
    Ok(())
}

// Directory seal: an excluded ancestor dir seals its subtree — a deeper `!`
// cannot rescue contents under it (Git semantics).
#[test]
fn excluded_dir_seals_against_deeper_reinclude() -> TestResult {
    let fixture =
        Fixture::new_with_gitignores("seal", &[("", "build/\n"), ("build", "!keep.txt\n")])?;
    assert!(is_ignored(&fixture, "build/keep.txt")?);
    Ok(())
}

// Composite ruleset: the N2/N3/nested/seal behaviors above hold together on
// one fixture, including the `.git` drop.
#[test]
fn composite_ruleset_routes_each_path_as_expected() -> TestResult {
    let fixture = Fixture::new_with_gitignores(
        "composite_routes",
        &[
            ("", "node_modules/\nlogs/*.log\nbuild/\n"),
            ("build", "!keep.txt\n"),
        ],
    )?;
    for (path, expected) in [
        ("frontend/node_modules/index.js", Route::Direct), // N2 dir-only any depth
        ("logs/sub/x.log", Route::Gated),                  // N3 star not crossing /
        ("logs/app.log", Route::Direct),
        ("build/keep.txt", Route::Direct), // seal beats deeper !
        ("src/main.rs", Route::Gated),
        (".git/config", Route::Drop),
    ] {
        assert_eq!(route_of(&fixture, path)?, expected, "route for {path}");
    }
    Ok(())
}

// Overlay/layerstack composition: a `.gitignore` published into an *upper*
// layer (the base layer carries none) is resolved through the active merged
// manifest — the same newest-layer-wins, whiteout-aware view the overlay
// mount projects. Proves the oracle reads `.gitignore` via `read_bytes`/
// `MergedView` across layers, not just from a single seeded layer.
#[test]
fn gitignore_resolves_through_published_upper_layer() -> TestResult {
    let fixture = Fixture::new("cross_layer")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[
        LayerChange::Write {
            path: lp(".gitignore")?,
            content: b"node_modules/\n".to_vec(),
        },
        LayerChange::Write {
            path: lp("frontend/.gitignore")?,
            content: b"dist/\n".to_vec(),
        },
    ])?;
    // Root rule from the upper layer, matched at depth via the seal.
    assert!(is_ignored(&fixture, "frontend/node_modules/index.js")?);
    // Nested rule, also published into the upper layer.
    assert!(is_ignored(&fixture, "frontend/dist/bundle.js")?);
    assert!(!is_ignored(&fixture, "src/main.rs")?);
    Ok(())
}

// Regression (double-strip on prefix replay, data-loss-class): a per-level
// matcher for dir `D` must not strip `D` from a path whose next component
// repeats `D`'s name. The caller already makes the path relative to `D`, so
// the matcher must be rooted at `.` — `GitignoreBuilder::new(D)` would strip
// `D` a SECOND time (raw byte prefix), turning `a/x` into `x` and matching an
// anchored `/x`. Ground truth below is `git check-ignore --no-index`.
#[test]
fn nested_anchored_pattern_not_double_stripped_on_prefix_replay() -> TestResult {
    let fixture = Fixture::new_with_gitignores(
        "prefix_replay",
        &[("a", "/x\n/b\n"), ("build", "/build/x\n")],
    )?;
    // `/x` anchored at `a/` matches `a/x` (DIRECT) but NOT `a/a/x` — routing
    // the tracked `a/a/x` DIRECT would bypass the gate and silently clobber.
    assert!(is_ignored(&fixture, "a/x")?);
    assert!(!is_ignored(&fixture, "a/a/x")?);
    // Seal variant: `/b` seals `a/b`'s subtree, but `a/a/b` is not the
    // anchored `a/b`, so its whole subtree must stay GATED.
    assert!(is_ignored(&fixture, "a/b/file.txt")?);
    assert!(!is_ignored(&fixture, "a/a/b/file.txt")?);
    // Opposite (false-GATED) direction: `/build/x` anchored at `build/` DOES
    // match `build/build/x`; the old double-strip dropped it to `x` and missed.
    assert!(is_ignored(&fixture, "build/build/x")?);
    assert!(!is_ignored(&fixture, "build/x")?);
    Ok(())
}
