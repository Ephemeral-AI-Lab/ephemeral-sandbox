use super::{command_environment, normalize_lexical, shell_argv, shell_cwd};
use crate::runner::protocol::NamespaceRunnerRequest;
use std::path::Path;

#[test]
fn shell_exec_command_string_uses_non_login_bash() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let argv = shell_argv(&request(serde_json::json!({"command": "echo hi"})))?;
    assert_eq!(
        argv,
        ["/bin/bash", "--noprofile", "--norc", "-c", "echo hi"]
            .map(str::to_owned)
            .to_vec()
    );
    Ok(())
}

#[test]
fn shell_exec_rejects_raw_argv() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let error = match shell_argv(&request(serde_json::json!({"command": ["echo", "hi"]}))) {
        Ok(argv) => {
            return Err(format!("shell_exec raw argv unexpectedly accepted: {argv:?}").into())
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("shell-format command string"));
    Ok(())
}

#[test]
fn shell_exec_rejects_external_cwd() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let external = format!("/tmp/namespace-external-cwd-{}", std::process::id());
    let rejected = request(serde_json::json!({"command": "pwd", "cwd": external}));
    let error = shell_cwd(&rejected).expect_err("external cwd should be rejected");
    assert!(error.to_string().contains("cwd escapes workspace"));
    Ok(())
}

#[test]
fn normalizes_paths_without_touching_fs() {
    assert_eq!(
        normalize_lexical(Path::new("/workspace/./a/../b")),
        Path::new("/workspace/b")
    );
}

#[test]
fn command_environment_forwards_proxy_vars_from_host() {
    // The Docker provider injects proxy vars into the container (hence the daemon)
    // environment via manager.docker.container_env. The namespace runner must
    // forward them so commands like `npm install` reach the registry through the
    // proxy rather than being stripped by the env allowlist.
    let key = "HTTPS_PROXY";
    let previous = std::env::var(key).ok();
    std::env::set_var(key, "http://proxy.test:3128");

    let env = command_environment(&serde_json::json!({}));

    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }

    assert_eq!(
        env.get("HTTPS_PROXY").map(String::as_str),
        Some("http://proxy.test:3128")
    );
}

fn request(args: serde_json::Value) -> NamespaceRunnerRequest {
    NamespaceRunnerRequest {
        request_id: "test".to_owned(),
        args,
        workspace_root: Path::new("/workspace").to_path_buf(),
        layer_paths: vec![],
        upperdir: None,
        workdir: None,
        ns_fds: None,
        timeout_seconds: None,
        trace: None,
        parent: None,
        observability_log_path: None,
    }
}
