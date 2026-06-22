//! Argv, cwd, and environment construction for namespace command execution.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::runner::protocol::NamespaceRunnerRequest;
use crate::runner::RunnerError;

pub(crate) fn shell_argv(request: &NamespaceRunnerRequest) -> Result<Vec<String>, RunnerError> {
    let shell_args = &request.args;
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
        "shell execution requires a shell-format command string".to_owned(),
    ))
}

pub(crate) fn shell_cwd(request: &NamespaceRunnerRequest) -> Result<PathBuf, RunnerError> {
    let raw = request
        .args
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");
    let workspace_root = normalize_lexical(&request.workspace_root);
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        let candidate = normalize_lexical(&candidate);
        if candidate.starts_with(&workspace_root) {
            let rel = candidate.strip_prefix(&workspace_root).map_err(|_| {
                RunnerError::InvalidRequest(format!(
                    "cwd escapes workspace replacement root: {raw}"
                ))
            })?;
            workspace_root.join(rel)
        } else {
            return Err(RunnerError::InvalidRequest(format!(
                "cwd escapes workspace replacement root: {raw}"
            )));
        }
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

pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

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

    const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

    let mut env = BTreeMap::new();
    for key in HOST_KEYS {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_owned(), value);
        }
    }
    let suffix = env
        .get("PATH")
        .filter(|path| !path.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_PATH.to_owned());
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
