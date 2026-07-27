use std::path::Path;

use xtask::package::{
    daemon_build_arguments, isolated_package_target_dir, storage_admin_build_arguments,
    DAEMON_PACKAGE, PACKAGE_TARGET_SUBDIRECTORY, STORAGE_ADMIN_BINARY, STORAGE_ADMIN_PACKAGE,
};

#[test]
fn packaged_daemon_always_enables_bounded_allocator() {
    assert_eq!(
        daemon_build_arguments("aarch64-unknown-linux-musl", "package-fast"),
        [
            "-p",
            DAEMON_PACKAGE,
            "--features",
            "jemalloc",
            "--target",
            "aarch64-unknown-linux-musl",
            "--profile",
            "package-fast",
        ]
    );
}

#[test]
fn packaged_storage_admin_builds_only_the_frozen_helper_binary() {
    assert_eq!(
        storage_admin_build_arguments("aarch64-unknown-linux-musl", "package-fast"),
        [
            "-p",
            STORAGE_ADMIN_PACKAGE,
            "--bin",
            STORAGE_ADMIN_BINARY,
            "--target",
            "aarch64-unknown-linux-musl",
            "--profile",
            "package-fast",
        ]
    );
}

#[test]
fn packaged_daemon_uses_an_isolated_cargo_target_directory() {
    assert_eq!(
        isolated_package_target_dir(Path::new("/workspace/target")),
        Path::new("/workspace/target").join(PACKAGE_TARGET_SUBDIRECTORY),
    );
}
