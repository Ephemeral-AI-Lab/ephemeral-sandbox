//! Argv, cwd, and environment construction for fresh-ns command execution.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::runner::error::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::path::normalize_lexical;
#[cfg(target_os = "linux")]
use eos_cas::RunRequest;

#[cfg(target_os = "linux")]
pub(super) fn plugin_service_argv(request: &RunRequest) -> Result<Vec<String>, RunnerError> {
    let Some(command) = request.tool_call.args.get("command") else {
        return Err(RunnerError::InvalidRequest(
            "plugin_service requires command argv".to_owned(),
        ));
    };
    let parts = command.as_array().ok_or_else(|| {
        RunnerError::InvalidRequest("plugin_service command must be an argv list".to_owned())
    })?;
    if parts.is_empty() {
        return Err(RunnerError::InvalidRequest(
            "plugin_service command argv must not be empty".to_owned(),
        ));
    }
    let argv: Result<Vec<String>, RunnerError> = parts
        .iter()
        .map(|part| {
            part.as_str().map_or_else(
                || {
                    Err(RunnerError::InvalidRequest(
                        "plugin_service command argv entries must be strings".to_owned(),
                    ))
                },
                |value| Ok(value.to_owned()),
            )
        })
        .collect();
    let argv = argv?;
    if argv[0].trim().is_empty() {
        return Err(RunnerError::InvalidRequest(
            "plugin_service command argv[0] must not be empty".to_owned(),
        ));
    }
    Ok(argv)
}

#[cfg(target_os = "linux")]
pub(super) fn shell_argv(request: &RunRequest) -> Result<Vec<String>, RunnerError> {
    let shell_args = &request.tool_call.args;
    let Some(command) = shell_args.get("command") else {
        return Err(RunnerError::InvalidRequest(
            "shell args require command".to_owned(),
        ));
    };
    if let Some(value) = command.as_str() {
        let command = value.trim();
        if command.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "shell command string must not be empty".to_owned(),
            ));
        }
        return Ok(vec![
            "/bin/bash".to_owned(),
            "--noprofile".to_owned(),
            "--norc".to_owned(),
            "-c".to_owned(),
            value.to_owned(),
        ]);
    }
    Err(RunnerError::InvalidRequest(
        "exec_command requires a shell-format command string".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
pub(super) fn shell_cwd(request: &RunRequest) -> Result<PathBuf, RunnerError> {
    let raw = request
        .tool_call
        .args
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");
    let workspace_root = normalize_lexical(&request.workspace_root.0);
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        let candidate = normalize_lexical(&candidate);
        let rel = candidate.strip_prefix(&workspace_root).map_err(|_| {
            RunnerError::InvalidRequest(format!("cwd escapes workspace replacement root: {raw}"))
        })?;
        workspace_root.join(rel)
    } else {
        normalize_lexical(&workspace_root.join(candidate))
    };
    if !resolved.starts_with(&workspace_root) {
        return Err(RunnerError::InvalidRequest(format!(
            "cwd escapes workspace replacement root: {raw}"
        )));
    }
    fs::create_dir_all(&resolved).map_err(RunnerError::Child)?;
    Ok(resolved)
}

#[cfg(target_os = "linux")]
pub(super) fn command_environment(args: &serde_json::Value) -> BTreeMap<String, String> {
    const HOST_KEYS: &[&str] = &["PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "TZ"];
    const RESTRICTED: &[&str] = &[
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "PATH",
        "PYTHONPATH",
        "BASH_ENV",
        "ENV",
    ];

    let mut env = BTreeMap::new();
    for key in HOST_KEYS {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_owned(), value);
        }
    }
    if !env.contains_key("PATH") {
        env.insert(
            "PATH".to_owned(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        );
    }
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let suffix = if existing_path.is_empty() {
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned()
    } else {
        existing_path
    };
    env.insert(
        "PATH".to_owned(),
        format!("/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:{suffix}"),
    );
    if let Some(extra) = args.get("env").and_then(serde_json::Value::as_object) {
        for (key, value) in extra {
            if !RESTRICTED.contains(&key.as_str()) {
                env.insert(
                    key.to_owned(),
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned),
                );
            }
        }
    }
    env.insert("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned());
    env
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{plugin_service_argv, shell_argv};
    use crate::runner::path::normalize_lexical;
    use eos_cas::Intent;
    use eos_cas::{RunMode, RunRequest, RunnerVerb, ToolCall, WorkspaceRoot};
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn exec_command_string_uses_non_login_bash() -> TestResult {
        let argv = shell_argv(&request(
            "exec_command",
            serde_json::json!({"command": "echo hi"}),
        ))?;
        assert_eq!(
            argv,
            ["/bin/bash", "--noprofile", "--norc", "-c", "echo hi"]
                .map(str::to_owned)
                .to_vec()
        );
        Ok(())
    }

    #[test]
    fn exec_command_rejects_raw_argv() -> TestResult {
        let error = match shell_argv(&request(
            "exec_command",
            serde_json::json!({"command": ["echo", "hi"]}),
        )) {
            Ok(argv) => {
                return Err(format!("exec_command raw argv unexpectedly accepted: {argv:?}").into())
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("shell-format command string"));
        Ok(())
    }

    #[test]
    fn plugin_service_requires_argv_command() -> TestResult {
        let argv = plugin_service_argv(&request(
            "plugin_service",
            serde_json::json!({"command": ["python3", "/eos/plugin/harness.py"]}),
        ))?;
        assert_eq!(
            argv,
            ["python3", "/eos/plugin/harness.py"]
                .map(str::to_owned)
                .to_vec()
        );

        let error = match plugin_service_argv(&request(
            "plugin_service",
            serde_json::json!({"command": "python3 /eos/plugin/harness.py"}),
        )) {
            Ok(argv) => {
                return Err(format!(
                    "plugin_service string command unexpectedly accepted: {argv:?}"
                )
                .into());
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("argv list"));
        Ok(())
    }

    #[test]
    fn normalizes_paths_without_touching_fs() {
        assert_eq!(
            normalize_lexical(Path::new("/workspace/./a/../b")),
            Path::new("/workspace/b")
        );
    }

    fn request(verb: &str, args: serde_json::Value) -> RunRequest {
        RunRequest {
            mode: RunMode::FreshNs,
            tool_call: ToolCall {
                invocation_id: "test".to_owned(),
                caller_id: "caller".to_owned(),
                verb: RunnerVerb::from(verb),
                intent: Intent::WriteAllowed,
                args,
                background: false,
            },
            workspace_root: WorkspaceRoot(Path::new("/workspace").to_path_buf()),
            layer_paths: vec![],
            upperdir: None,
            workdir: None,
            ns_fds: None,
            cgroup_path: None,
            timeout_seconds: None,
        }
    }
}
