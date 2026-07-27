use std::path::{Path, PathBuf};

pub const DAEMON_PACKAGE: &str = "sandbox-daemon";
pub const STORAGE_ADMIN_PACKAGE: &str = "sandbox-runtime-mpla-poc";
pub const STORAGE_ADMIN_BINARY: &str = "mpla-storage-admin-v1";
pub const PACKAGE_TARGET_SUBDIRECTORY: &str = "xtask-package";

pub fn daemon_build_arguments<'a>(target: &'a str, profile: &'a str) -> [&'a str; 8] {
    [
        "-p",
        DAEMON_PACKAGE,
        "--features",
        "jemalloc",
        "--target",
        target,
        "--profile",
        profile,
    ]
}

pub fn storage_admin_build_arguments<'a>(target: &'a str, profile: &'a str) -> [&'a str; 8] {
    [
        "-p",
        STORAGE_ADMIN_PACKAGE,
        "--bin",
        STORAGE_ADMIN_BINARY,
        "--target",
        target,
        "--profile",
        profile,
    ]
}

pub fn isolated_package_target_dir(cargo_target_dir: &Path) -> PathBuf {
    cargo_target_dir.join(PACKAGE_TARGET_SUBDIRECTORY)
}
