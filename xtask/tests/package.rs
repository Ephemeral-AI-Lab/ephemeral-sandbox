use std::path::Path;

use xtask::package::{
    daemon_build_arguments, isolated_package_target_dir, DAEMON_PACKAGE,
    PACKAGE_TARGET_SUBDIRECTORY,
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
fn packaged_daemon_uses_an_isolated_cargo_target_directory() {
    assert_eq!(
        isolated_package_target_dir(Path::new("/workspace/target")),
        Path::new("/workspace/target").join(PACKAGE_TARGET_SUBDIRECTORY),
    );
}
