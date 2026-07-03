#![deny(unsafe_op_in_unsafe_fn)]

#[path = "../src/holder/mod.rs"]
pub mod holder;
#[path = "../src/runner/mod.rs"]
pub mod runner;

pub(crate) use holder::network::parse_network_config;
pub(crate) use holder::Handshake;

#[cfg(target_os = "linux")]
pub(crate) use runner::shell_exec::request::{
    command_environment, normalize_lexical, shell_argv, shell_cwd,
};

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
            "setns mount namespace failed",
        ));

        let message = error.to_string();
        assert!(message.contains("namespace syscall failed"));
        assert!(message.contains("setns mount namespace failed"));
    }

    #[test]
    fn runner_overlay_error_display_includes_mount_syscall_context() {
        let error = super::runner::RunnerError::Overlay(
            sandbox_runtime_overlay::OverlayError::MountSyscall {
                context: "fsconfig lowerdir+",
                source: std::io::Error::from_raw_os_error(libc::EINVAL),
            },
        );

        let message = error.to_string();
        assert!(message.contains("overlay mount failed"));
        assert!(message.contains("fsconfig lowerdir+"));
        assert!(message.contains("Invalid argument") || message.contains("EINVAL"));
    }

    #[test]
    fn run_result_has_no_runner_trace_transport_field() {
        let value = serde_json::to_value(super::runner::protocol::RunResult {
            exit_code: 0,
            payload: serde_json::json!({ "status": "ok" }),
        })
        .expect("run result serializes");

        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["payload"]["status"], "ok");
        assert!(value.get("runner_trace").is_none());
    }

    #[test]
    fn shell_security_field_is_rejected_on_runner_wire() {
        let error = serde_json::from_value::<super::runner::protocol::NamespaceRunnerRequest>(
            serde_json::json!({
                "request_id": "req-off",
                "args": {},
                "workspace_root": "/workspace",
                "layer_paths": [],
                "shell_security": { "mode": "off" }
            }),
        )
        .expect_err("shell_security should not be part of the runner wire protocol");

        assert!(error.to_string().contains("shell_security"));
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
mod runner_shell_security_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/runner/shell_security.rs"
    ));
}

#[cfg(target_os = "linux")]
mod runner_shell_exec_request_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/runner/shell_exec/request.rs"
    ));
}

#[cfg(target_os = "linux")]
mod runner_shell_exec_execute_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/runner/shell_exec/execute.rs"
    ));
}

#[cfg(not(target_os = "linux"))]
mod runner_non_linux_tests {
    #[test]
    fn runner_live_namespace_checks_are_linux_gated() {
        let linux_ostype =
            std::fs::read_to_string("/proc/sys/kernel/ostype").unwrap_or_else(|_| String::new());
        assert_ne!(linux_ostype.trim(), "Linux");
    }
}
