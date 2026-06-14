use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use config::configs::daemon::PyrightLspConfig;

use crate::PluginRuntimeError;

const PYRIGHT_PYPI_VERSION: &str = "1.1.410";

pub(super) fn resolve_pyright_command(
    config: &PyrightLspConfig,
) -> Result<Vec<String>, PluginRuntimeError> {
    if config.node_path.exists() && config.pyright_langserver_path.exists() {
        return Ok(vec![
            config.node_path.to_string_lossy().into_owned(),
            config
                .pyright_langserver_path
                .to_string_lossy()
                .into_owned(),
            "--stdio".to_owned(),
        ]);
    }
    if let Some(path) = find_executable("pyright-langserver") {
        return Ok(vec![
            path.to_string_lossy().into_owned(),
            "--stdio".to_owned(),
        ]);
    }
    for candidate in [
        "/usr/local/bin/pyright-langserver",
        "/usr/bin/pyright-langserver",
        "/opt/miniconda3/bin/pyright-langserver",
        "/opt/miniconda3/envs/testbed/bin/pyright-langserver",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Ok(vec![candidate.to_owned(), "--stdio".to_owned()]);
        }
    }
    if let Some(command) = provision_python_pyright_command(config)? {
        return Ok(command);
    }
    Err(PluginRuntimeError::PyrightLsp(format!(
        "pyright_lsp could not find configured node/langserver ({}, {}) or pyright-langserver in PATH",
        config.node_path.display(),
        config.pyright_langserver_path.display()
    )))
}

fn provision_python_pyright_command(
    config: &PyrightLspConfig,
) -> Result<Option<Vec<String>>, PluginRuntimeError> {
    let Some(python) = find_python() else {
        return Ok(None);
    };
    let root = pyright_python_root(config)?;
    let marker = root.join("pyright").join("langserver.py");
    if !marker.exists() {
        fs::create_dir_all(&root).map_err(|err| {
            PluginRuntimeError::PyrightLsp(format!(
                "create pyright_lsp Python asset root {}: {err}",
                root.display()
            ))
        })?;
        let output = Command::new(&python)
            .args([
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "--no-input",
                "--target",
            ])
            .arg(&root)
            .arg(format!("pyright=={PYRIGHT_PYPI_VERSION}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| {
                PluginRuntimeError::PyrightLsp(format!(
                    "install pyright_lsp Python assets with {}: {err}",
                    python.display()
                ))
            })?;
        if !output.status.success() {
            return Err(PluginRuntimeError::PyrightLsp(format!(
                "install pyright_lsp Python assets failed: {}",
                bounded_command_output(&output.stderr, &output.stdout)
            )));
        }
    }
    Ok(Some(vec![
        "/bin/sh".to_owned(),
        "-lc".to_owned(),
        format!(
            "PYTHONPATH={} exec {} -m pyright.langserver --stdio",
            shell_quote(&root),
            shell_quote(&python)
        ),
    ]))
}

fn pyright_python_root(config: &PyrightLspConfig) -> Result<PathBuf, PluginRuntimeError> {
    let parent = config.workspace_root.parent().ok_or_else(|| {
        PluginRuntimeError::PyrightLsp(format!(
            "pyright_lsp workspace_root has no parent: {}",
            config.workspace_root.display()
        ))
    })?;
    Ok(parent.join("python"))
}

fn find_python() -> Option<PathBuf> {
    find_executable("python3")
        .or_else(|| find_executable("python"))
        .or_else(|| {
            [
                "/opt/miniconda3/bin/python",
                "/usr/local/bin/python3",
                "/usr/bin/python3",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|candidate| candidate.exists())
        })
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn bounded_command_output(stderr: &[u8], stdout: &[u8]) -> String {
    let mut text = String::new();
    if !stderr.is_empty() {
        text.push_str("stderr=");
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    if !stdout.is_empty() {
        if !text.is_empty() {
            text.push_str("; ");
        }
        text.push_str("stdout=");
        text.push_str(&String::from_utf8_lossy(stdout));
    }
    const LIMIT: usize = 4096;
    if text.len() > LIMIT {
        text.truncate(LIMIT);
        text.push_str("...");
    }
    text
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.exists())
    })
}
