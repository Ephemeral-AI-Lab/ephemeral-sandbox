use super::{OverlayHandle, ValidatedMountInputs};
use std::path::PathBuf;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn mount_inputs_pin_only_lowerdirs_with_fd_paths() -> TestResult {
    let root = test_dir("workspace-root")?;
    let lower = test_dir("lower")?;
    let upperdir = test_dir("upper")?;
    let workdir = test_dir("work")?;
    let inputs = ValidatedMountInputs::open(
        &root,
        &OverlayHandle {
            upperdir: upperdir.clone(),
            workdir: workdir.clone(),
            layer_paths: vec![lower],
        },
    )?;

    assert!(inputs.layer_paths[0].starts_with("/proc/self/fd/"));
    assert_eq!(inputs.upperdir, upperdir);
    assert_eq!(inputs.workdir, workdir);
    Ok(())
}

fn test_dir(name: &str) -> TestResult<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "eos-overlay-kernel-mount-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
