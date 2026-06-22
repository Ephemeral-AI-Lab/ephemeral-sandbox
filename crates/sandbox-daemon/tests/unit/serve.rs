use std::path::PathBuf;

use anyhow::Result;
use sandbox_runtime_config::configs::daemon::{
    DaemonServeMode, DaemonServerConfig, TelemetryConfig, TelemetryOutputStream, TelemetrySink,
};

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
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
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
            "--workspace-root",
            "/testbed",
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
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
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
fn spawned_foreground_args_include_dynamic_sandbox_id() -> Result<()> {
    let config = DaemonCliConfig::parse(
        vec![
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
            "--sandbox-id".to_owned(),
            "sbox-1".to_owned(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    )?;

    assert_eq!(config.sandbox_id.as_deref(), Some("sbox-1"));
    assert_eq!(config.serve_mode(), DaemonServeMode::Spawn);
    assert!(
        config
            .foreground_args()
            .windows(2)
            .any(|window| window[0] == "--sandbox-id" && window[1] == "sbox-1"),
        "spawned foreground argv must carry dynamic sandbox identity"
    );
    Ok(())
}

#[test]
fn sandbox_id_must_be_non_empty() {
    let result = DaemonCliConfig::parse(
        vec![
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
            "--sandbox-id".to_owned(),
            " ".to_owned(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    );
    let error = match result {
        Ok(_) => panic!("blank sandbox id rejected"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "--sandbox-id must be non-empty");
}

#[test]
fn local_json_telemetry_is_foreground_only_for_serve_spawn() -> Result<()> {
    let config = DaemonCliConfig::parse(
        vec![
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    )?;
    let telemetry = local_json_telemetry();
    let error = telemetry
        .validate_for_serve_mode(config.serve_mode())
        .expect_err("spawned local json telemetry is rejected");

    assert_eq!(error.field, "daemon.telemetry.sink");
    Ok(())
}

#[test]
fn tcp_listener_requires_configured_auth_token() {
    let result = DaemonCliConfig::parse(
        vec![
            "--config-yaml".to_owned(),
            "/eos/custom/prd.yml".to_owned(),
            "--workspace-root".to_owned(),
            "/testbed".to_owned(),
            "--tcp-host".to_owned(),
            "0.0.0.0".to_owned(),
            "--tcp-port".to_owned(),
            "37777".to_owned(),
            "--auth-token".to_owned(),
            String::new(),
        ],
        &server_defaults(),
        PathBuf::from("/eos/custom/prd.yml"),
    );
    let error = match result {
        Ok(_) => panic!("tcp listener without a non-empty auth token is rejected"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "serve TCP listener requires --auth-token or SANDBOX_DAEMON_AUTH_TOKEN"
    );
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

fn local_json_telemetry() -> TelemetryConfig {
    TelemetryConfig {
        enabled: true,
        service_name: "sandbox-daemon".to_owned(),
        level: "info".to_owned(),
        sink: Some(TelemetrySink::LocalJson {
            stream: TelemetryOutputStream::Stdout,
        }),
    }
}
