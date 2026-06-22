use sandbox_runtime_workspace::profile::RemountOverlayResult;
use serde_json::json;

use crate::trace_capture::capture_traces;

#[test]
fn parses_successful_remount_overlay_result() {
    let result = RemountOverlayResult::from_payload(&json!({
        "mount_verified": true,
        "staged_switch": true,
        "staging_verified": true,
        "rollback_unmounted": true,
        "mountinfo_fs_type": "overlay",
        "mountinfo_lowerdir_count": 2,
        "mountinfo_lowerdir_expected_count": 2,
        "mountinfo_lowerdir_count_matched": true,
        "mountinfo_lowerdir_verified": null,
    }));

    assert!(result.mount_verified);
    assert_eq!(result.failure_summary, None);
}

#[test]
fn failure_summary_prioritizes_rollback_cleanup() {
    let result = RemountOverlayResult::from_payload(&json!({
        "mount_verified": false,
        "staged_switch": false,
        "staging_verified": true,
        "rollback_unmounted": false,
        "rollback_unmount_error": "busy",
    }));

    assert!(result
        .failure_summary()
        .contains("rollback cleanup failed: busy"));
}

#[test]
fn remount_overlay_result_emits_only_allowlisted_trace_facts() {
    let payload = json!({
        "mount_verified": false,
        "staged_switch": true,
        "staging_verified": true,
        "rollback_unmounted": false,
        "rollback_unmount_error": "RAW_REMOUNT_ERROR_SECRET",
        "mountinfo_fs_type": "RAW_FS_TYPE_SECRET",
        "mountinfo_lowerdir_count": 2,
        "mountinfo_lowerdir_expected_count": 3,
        "mountinfo_lowerdir_count_matched": false,
        "mountinfo_lowerdir_verified": false,
        "probe_path": "/workspace/REMOUNT_PATH_SECRET",
        "probe_content": "REMOUNT_CONTENT_SECRET",
        "probe_read_ok": false,
        "probe_content_matched": false,
    });

    let traces = capture_traces(|| {
        let result = RemountOverlayResult::from_payload(&payload);
        assert!(!result.mount_verified);
        assert!(result
            .failure_summary()
            .contains("rollback cleanup failed: RAW_REMOUNT_ERROR_SECRET"));
    });

    for expected in [
        "event workspace_remount_overlay_result",
        "mount_verified=false",
        "failure_reason=rollback_cleanup_failed",
        "lowerdir_count=2",
        "lowerdir_expected_count=3",
        "lowerdir_count_matched=false",
        "probe_read_ok=false",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }
    for forbidden in [
        "RAW_REMOUNT_ERROR_SECRET",
        "RAW_FS_TYPE_SECRET",
        "REMOUNT_PATH_SECRET",
        "REMOUNT_CONTENT_SECRET",
        "/workspace/",
        "RemountOverlayResult",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }
}
