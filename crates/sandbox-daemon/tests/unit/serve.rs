use std::path::PathBuf;

use anyhow::Result;
use sandbox_runtime_config::configs::daemon::DaemonServerConfig;

use crate::serve_cli::{daemon_config_path_arg, DaemonCliConfig};

fn server_defaults() -> DaemonServerConfig {
    DaemonServerConfig {
        socket_path: PathBuf::from("/eos/runtime/default.sock"),
        pid_path: PathBuf::from("/eos/runtime/default.pid"),
        max_worker_threads: 2,
    }
}

#[test]
fn config_yaml_flag_is_parsed_and_preserved_for_spawned_foreground() -> Result<()> {
    let config = DaemonCliConfig::parse(
        vec![
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--socket".to_owned(),
            "/eos/runtime/runtime.sock".to_owned(),
            "--pid-file".to_owned(),
            "/eos/runtime/runtime.pid".to_owned(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    )?;

    assert_eq!(
        config.config_yaml_path,
        PathBuf::from("/eos/custom/prd.yml")
    );
    assert_eq!(
        config.foreground_args(),
        vec![
            "serve",
            "--config-yaml",
            "/eos/custom/prd.yml",
            "--socket",
            "/eos/runtime/runtime.sock",
            "--pid-file",
            "/eos/runtime/runtime.pid",
        ]
    );
    Ok(())
}

#[test]
fn spawned_foreground_args_omit_auth_token() -> Result<()> {
    let config = DaemonCliConfig::parse(
        vec![
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--tcp-host".to_owned(),
            "0.0.0.0".to_owned(),
            "--tcp-port".to_owned(),
            "37777".to_owned(),
            "--auth-token".to_owned(),
            "token-1".to_owned(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    )?;

    assert_eq!(config.auth_token.as_deref(), Some("token-1"));
    assert!(
        !config.foreground_args().iter().any(|arg| matches!(
            arg.as_str(),
            "--auth-token" | "token-1"
        )),
        "auth token must be passed through the child environment, not argv"
    );
    Ok(())
}

#[test]
fn config_yaml_preparse_returns_explicit_path() -> Result<()> {
    assert_eq!(
        daemon_config_path_arg(&[
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            "/eos/config.yml".to_owned(),
        ])?,
        PathBuf::from("/eos/config.yml")
    );
    assert!(daemon_config_path_arg(&["--config-yaml".to_owned()]).is_err());
    Ok(())
}

#[test]
fn config_yaml_preparse_requires_explicit_path() {
    let err = daemon_config_path_arg(&["--spawn".to_owned()]).expect_err("config path required");
    assert_eq!(err.to_string(), "serve requires --config-yaml PATH");
}
