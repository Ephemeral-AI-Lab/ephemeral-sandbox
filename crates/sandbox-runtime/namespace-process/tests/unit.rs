#![deny(unsafe_op_in_unsafe_fn)]

#[path = "../src/holder/mod.rs"]
pub mod holder;
#[path = "../src/runner/mod.rs"]
pub mod runner;

pub(crate) use holder::network::parse_network_config;
pub(crate) use holder::Handshake;
#[cfg(target_os = "linux")]
pub(crate) use runner::setns::{
    join_cgroup, mountinfo_lowerdir_count_matched, mountinfo_lowerdir_verified,
    namespace_fd_order_with_types, remount_overlay, WorkspaceMountInfo,
};

#[cfg(target_os = "linux")]
pub(crate) use runner::shell_exec::request::{normalize_lexical, shell_argv, shell_cwd};

mod holder_handshake_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/holder/handshake.rs"
    ));
}

mod holder_network_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/holder/network.rs"
    ));
}

mod runner_error_tests {
    #[test]
    fn runner_syscall_error_display_includes_source_context() {
        let error = super::runner::RunnerError::Syscall(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "join cgroup /sys/fs/cgroup/eos/sessions/ws-1/commands/cmd-1 via cgroup.procs failed",
        ));

        let message = error.to_string();
        assert!(message.contains("namespace syscall failed"));
        assert!(message.contains("/sys/fs/cgroup/eos/sessions/ws-1/commands/cmd-1"));
        assert!(message.contains("cgroup.procs"));
    }
}

#[cfg(target_os = "linux")]
mod runner_setns_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/runner/setns.rs"
    ));
}

#[cfg(target_os = "linux")]
mod runner_shell_exec_request_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/runner/shell_exec/request.rs"
    ));
}

#[cfg(not(target_os = "linux"))]
mod runner_non_linux_tests {
    #[test]
    fn runner_live_cgroup_checks_are_linux_gated() {
        let linux_ostype =
            std::fs::read_to_string("/proc/sys/kernel/ostype").unwrap_or_else(|_| String::new());
        assert_ne!(linux_ostype.trim(), "Linux");
    }
}
