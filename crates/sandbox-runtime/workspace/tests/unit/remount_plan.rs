use sandbox_runtime_workspace::profile::RemountOverlayResult;
use serde_json::json;

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
