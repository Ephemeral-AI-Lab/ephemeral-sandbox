use std::ffi::OsString;

use super::{
    container_copy_target, daemon_spawn_args, docker_display, docker_exec_args, docker_run_args,
    parse_published_addr, redact_docker_error_text, validate_remote_name, ContainerLifetime,
    ContainerSpec,
};

#[test]
fn copy_target_uses_requested_remote_name() {
    assert_eq!(
        container_copy_target("box", "/eos/runtime/daemon/", "eosd"),
        "box:/eos/runtime/daemon/eosd"
    );
    assert!(validate_remote_name("eosd").is_ok());
    assert!(validate_remote_name("../eosd").is_err());
}

#[test]
fn docker_exec_args_runs_from_root_after_leading_flags() {
    assert_eq!(
        docker_exec_args("box", &["mkdir", "-p", "/testbed"]),
        vec!["exec", "-w", "/", "box", "mkdir", "-p", "/testbed"]
    );
    assert_eq!(
        docker_exec_args("box", &["-d", "/eos/runtime/daemon/eosd", "daemon"]),
        vec![
            "exec",
            "-d",
            "-w",
            "/",
            "box",
            "/eos/runtime/daemon/eosd",
            "daemon"
        ]
    );
    assert_eq!(
        docker_exec_args(
            "box",
            &[
                "-e",
                "EOS_DAEMON_AUTH_TOKEN=token-1",
                "-d",
                "/eos/runtime/daemon/eosd",
                "daemon",
            ],
        ),
        vec![
            "exec",
            "-e",
            "EOS_DAEMON_AUTH_TOKEN=token-1",
            "-d",
            "-w",
            "/",
            "box",
            "/eos/runtime/daemon/eosd",
            "daemon"
        ]
    );
}

#[test]
fn docker_run_args_honor_privileged_bootstrap_config() {
    let privileged = docker_run_args(&container_spec(true), 37_657);
    assert!(
        privileged.iter().any(|arg| arg == "--privileged"),
        "privileged bootstrap should pass --privileged: {privileged:?}"
    );

    let unprivileged = docker_run_args(&container_spec(false), 37_657);
    assert!(
        !unprivileged.iter().any(|arg| arg == "--privileged"),
        "unprivileged bootstrap must omit --privileged: {unprivileged:?}"
    );
}

#[test]
fn daemon_spawn_args_pass_remote_config_path_to_eosd() {
    let args = daemon_spawn_args(
        "/eos/runtime/eosd",
        "/eos/runtime",
        "/eos/custom/prd.yml",
        37_777,
        "token-1",
        "forward-token-1",
    );
    assert_eq!(
        args,
        vec![
            "-e",
            "EOS_DAEMON_AUTH_TOKEN=token-1",
            "-e",
            "EOS_DAEMON_FORWARD_AUTH_TOKEN=forward-token-1",
            "-d",
            "/eos/runtime/eosd",
            "daemon",
            "--spawn",
            "--config-yaml",
            "/eos/custom/prd.yml",
            "--socket",
            "/eos/runtime/runtime.sock",
            "--pid-file",
            "/eos/runtime/runtime.pid",
            "--log-file",
            "/eos/runtime/runtime.log",
            "--tcp-host",
            "0.0.0.0",
            "--tcp-port",
            "37777",
        ]
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--auth-token" || arg == "--forward-auth-token"),
        "daemon token must not be exposed in eosd argv: {args:?}"
    );
}

#[test]
fn docker_display_redacts_daemon_auth_tokens() {
    let display = docker_display(&[
        OsString::from("exec"),
        OsString::from("-e"),
        OsString::from("EOS_DAEMON_AUTH_TOKEN=token-1"),
        OsString::from("-e"),
        OsString::from("EOS_DAEMON_FORWARD_AUTH_TOKEN=forward-token-1"),
        OsString::from("box"),
    ]);

    assert!(display.contains("EOS_DAEMON_AUTH_TOKEN=<redacted>"));
    assert!(display.contains("EOS_DAEMON_FORWARD_AUTH_TOKEN=<redacted>"));
    assert!(!display.contains("token-1"), "{display}");
    assert!(!display.contains("forward-token-1"), "{display}");
}

#[test]
fn docker_error_text_redacts_daemon_auth_tokens() {
    let redacted = redact_docker_error_text(
        "docker failed: -e EOS_DAEMON_AUTH_TOKEN=daemon-secret -e EOS_DAEMON_FORWARD_AUTH_TOKEN=forward-secret",
    );

    assert!(redacted.contains("EOS_DAEMON_AUTH_TOKEN=<redacted>"));
    assert!(redacted.contains("EOS_DAEMON_FORWARD_AUTH_TOKEN=<redacted>"));
    assert!(!redacted.contains("daemon-secret"), "{redacted}");
    assert!(!redacted.contains("forward-secret"), "{redacted}");
}

fn container_spec(privileged: bool) -> ContainerSpec {
    ContainerSpec {
        name: "box".to_owned(),
        image: "sandbox:latest".to_owned(),
        platform: Some("linux/amd64".to_owned()),
        privileged,
        cap_add: vec!["SYS_ADMIN".to_owned()],
        security_opt: vec!["seccomp=unconfined".to_owned()],
        tmpfs: vec!["/eos/state:rw,size=1g".to_owned()],
        labels: vec![("owner".to_owned(), "test".to_owned())],
        lifetime: ContainerLifetime::Keep,
    }
}

#[test]
fn published_addr_parses_loopback_port() {
    assert_eq!(
        parse_published_addr("0.0.0.0:54321"),
        Some("127.0.0.1:54321".parse().expect("addr"))
    );
    assert_eq!(parse_published_addr("garbage"), None);
}
