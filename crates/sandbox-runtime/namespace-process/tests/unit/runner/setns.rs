use crate::runner::protocol::{Fd, NamespaceRunnerRequest, NsFds};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
#[test]
fn remount_overlay_requires_setns_payload() -> Result<(), Box<dyn std::error::Error>> {
    let mut request = request(None);
    request.layer_paths = vec![Path::new("/tmp/layer").to_path_buf()];
    let hidden_paths = [Path::new("/eos").to_path_buf()];

    let Err(error) = super::remount_overlay(&request, &hidden_paths) else {
        return Err("remount overlay should require setns namespace fds".into());
    };

    assert!(error.to_string().contains("requires ns_fds"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn namespace_order_is_user_mnt_pid_net_and_skips_missing_fds() {
    let ns_fds = NsFds {
        user: Some(Fd(10)),
        mnt: Some(Fd(11)),
        pid: None,
        net: Some(Fd(12)),
    };
    let order: Vec<(&str, i32)> = super::namespace_fd_order_with_types(&ns_fds)
        .into_iter()
        .map(|(name, fd, _nstype)| (name, fd))
        .collect();
    assert_eq!(order, vec![("user", 10), ("mnt", 11), ("net", 12)]);
}

#[test]
fn runner_request_carries_cgroup_path_as_input_metadata() {
    let mut request = request(None);
    request.cgroup_path = Some(Path::new("/tmp/eos-cgroup-test").to_path_buf());

    assert_eq!(
        request.cgroup_path.as_deref(),
        Some(Path::new("/tmp/eos-cgroup-test"))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn runner_cgroup_join_error_labels_requested_cgroup_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = request(None);
    request.cgroup_path = Some(Path::new("/tmp/eos-missing-cgroup-for-test").to_path_buf());

    let error = super::join_cgroup(&request).expect_err("missing cgroup.procs should fail");
    let message = error.to_string();
    assert!(message.contains("join cgroup"), "{message}");
    assert!(message.contains("cgroup.procs"), "{message}");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn lowerdir_verification_reports_only_available_kernel_proof() {
    let expected = vec![PathBuf::from("/layers/l4"), PathBuf::from("/layers/parent")];
    assert_eq!(
        super::mountinfo_lowerdir_count_matched(None, expected.len()),
        None
    );
    assert_eq!(super::mountinfo_lowerdir_verified(None, &expected), None);

    let hidden = super::WorkspaceMountInfo {
        mount_point: "/workspace".to_owned(),
        fs_type: "overlay".to_owned(),
        lowerdir_count: None,
        lowerdir: None,
    };
    assert_eq!(
        super::mountinfo_lowerdir_count_matched(Some(&hidden), expected.len()),
        None
    );
    assert_eq!(
        super::mountinfo_lowerdir_verified(Some(&hidden), &expected),
        None
    );

    let count_only = super::WorkspaceMountInfo {
        lowerdir_count: Some(2),
        ..hidden.clone()
    };
    assert_eq!(
        super::mountinfo_lowerdir_count_matched(Some(&count_only), expected.len()),
        Some(true)
    );
    assert_eq!(
        super::mountinfo_lowerdir_verified(Some(&count_only), &expected),
        None
    );

    let exact = super::WorkspaceMountInfo {
        lowerdir_count: Some(2),
        lowerdir: Some("/layers/l4:/layers/parent".to_owned()),
        ..hidden.clone()
    };
    assert_eq!(
        super::mountinfo_lowerdir_verified(Some(&exact), &expected),
        Some(true)
    );

    let mismatch = super::WorkspaceMountInfo {
        lowerdir: Some("/layers/parent:/layers/l4".to_owned()),
        ..exact
    };
    assert_eq!(
        super::mountinfo_lowerdir_verified(Some(&mismatch), &expected),
        Some(false)
    );
}

fn request(ns_fds: Option<NsFds>) -> NamespaceRunnerRequest {
    NamespaceRunnerRequest {
        request_id: "test".to_owned(),
        args: serde_json::json!({"command": "true"}),
        workspace_root: Path::new("/workspace").to_path_buf(),
        layer_paths: vec![],
        upperdir: Some(Path::new("/tmp/iws/upper").to_path_buf()),
        workdir: Some(Path::new("/tmp/iws/work").to_path_buf()),
        ns_fds,
        cgroup_path: None,
        timeout_seconds: None,
    }
}
