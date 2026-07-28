use super::{OverlayHandle, ValidatedMountInputs};
use std::path::PathBuf;

#[test]
fn mount_inputs_pin_only_lowerdirs_with_fd_paths(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = test_dir("workspace-root")?;
    let lower = test_dir("lower")?;
    let upperdir = test_dir("upper")?;
    let workdir = test_dir("work")?;
    let inputs = ValidatedMountInputs::open(
        &root,
        &OverlayHandle {
            upperdir: upperdir.clone(),
            workdir: workdir.clone(),
            layer_paths: vec![lower.clone()],
        },
    )?;

    assert!(inputs.layer_paths[0].starts_with("/proc/self/fd/"));
    assert_eq!(inputs.lower_bindings.len(), 1);
    assert_eq!(inputs.lower_bindings[0].index, 0);
    assert_eq!(inputs.lower_bindings[0].authorized_path, lower);
    assert_eq!(
        inputs.lower_bindings[0].fd_identity,
        inputs.lower_bindings[0].authorized_path_identity
    );
    assert_eq!(inputs.upperdir, upperdir);
    assert_eq!(inputs.workdir, workdir);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_lowerdir_value_preserves_newest_first_order() {
    let lowerdirs = vec![
        PathBuf::from("/proc/self/fd/10"),
        PathBuf::from("/proc/self/fd/11"),
        PathBuf::from("/proc/self/fd/12"),
    ];

    assert_eq!(
        super::legacy_lowerdir_value(&lowerdirs),
        "/proc/self/fd/10:/proc/self/fd/11:/proc/self/fd/12"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn raw_overlay_mount_requires_nodev_and_nosuid_before_attach() {
    use rustix::mount::MountAttrFlags;

    let attributes = super::overlay_mount_attributes();
    assert!(attributes.contains(MountAttrFlags::MOUNT_ATTR_NODEV));
    assert!(attributes.contains(MountAttrFlags::MOUNT_ATTR_NOSUID));
    assert_eq!(attributes.bits(), 0x0000_0006);
}

fn test_dir(name: &str) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let path = std::env::temp_dir().join(format!(
        "overlay-kernel-mount-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

#[cfg(target_os = "linux")]
#[test]
fn strict_unmount_surfaces_kernel_errno_verbatim_with_no_fallback() {
    // A plain directory is not a mountpoint: umount2 reports EINVAL and the
    // helper must surface it verbatim instead of falling back to a lazy
    // detach (the peel helper's EINVAL-means-done logic must NOT be copied).
    let dir = test_dir("strict-einval").expect("test dir");
    let error = super::strict_unmount(&dir).expect_err("plain dir is not a mount");
    match error {
        super::OverlayError::MountSyscall { context, source } => {
            assert_eq!(context, "strict umount");
            assert!(
                matches!(source.raw_os_error(), Some(code) if code == 22 || code == 1),
                "EINVAL (or EPERM for unprivileged runners) surfaced verbatim, got {source:?}"
            );
        }
        other => panic!("unexpected error shape: {other:?}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn move_mountpoint_of_a_non_mount_fails_cleanly() {
    let dir = test_dir("move-nonmount").expect("test dir");
    let source = std::fs::File::open(&dir).expect("open source");
    let target_dir = test_dir("move-target").expect("target dir");
    let target = std::fs::File::open(&target_dir).expect("open target");
    let error = super::move_mountpoint(&source, &target)
        .expect_err("a plain directory fd is not a movable mount root");
    assert!(matches!(error, super::OverlayError::MountSyscall { .. }));
}
