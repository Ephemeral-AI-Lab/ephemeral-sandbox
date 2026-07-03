use crate::runner::protocol::{Fd, NsFds};

#[cfg(target_os = "linux")]
#[test]
fn namespace_order_is_user_mnt_pid_net_and_skips_missing_fds() {
    let ns_fds = NsFds {
        user: Some(Fd(10)),
        mnt: Some(Fd(11)),
        pid: None,
        net: Some(Fd(12)),
    };
    let order: Vec<(&str, i32)> =
        crate::runner::setns::namespaces::namespace_fd_order_with_types(&ns_fds)
            .into_iter()
            .map(|(name, fd, _nstype)| (name, fd))
            .collect();
    assert_eq!(order, vec![("user", 10), ("mnt", 11), ("net", 12)]);
}

// The remount report contract: two booleans plus free-form detail, present
// on every constructor path — C5 outcomes are a pure function of them.
#[cfg(target_os = "linux")]
#[test]
fn remount_report_always_carries_two_booleans_and_detail() {
    for (first, verified, detail) in [
        (false, false, "stage_failed:staging_mount:28"),
        (false, false, "move_failed:first_move:22"),
        (true, false, "mount_uncertain:second_move:22"),
        (true, true, crate::runner::setns::remount_overlay::DETAIL_SWITCHED),
        (
            true,
            true,
            crate::runner::setns::remount_overlay::DETAIL_ROLLBACK_UNMOUNT_BUSY,
        ),
    ] {
        let result = crate::runner::setns::remount_overlay::report(first, verified, detail);
        assert_eq!(result.exit_code, 0, "reports ride the payload, not the exit code");
        assert_eq!(result.payload["first_move_succeeded"], serde_json::json!(first));
        assert_eq!(result.payload["mount_verified"], serde_json::json!(verified));
        assert_eq!(result.payload["detail"], serde_json::json!(detail));
    }
}

// mount_verified=true only ever pairs with first_move_succeeded=true in the
// runner body; the park detail is the exact string the daemon's C5 policy
// matches on.
#[cfg(target_os = "linux")]
#[test]
fn remount_park_detail_is_the_c5_contract_string() {
    assert_eq!(
        crate::runner::setns::remount_overlay::DETAIL_ROLLBACK_UNMOUNT_BUSY,
        "pinned:rollback_unmount_busy"
    );
    assert_eq!(crate::runner::setns::remount_overlay::DETAIL_SWITCHED, "switched");
}
