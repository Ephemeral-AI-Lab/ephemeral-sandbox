use std::ffi::OsString;
use std::path::PathBuf;

use sandbox_operation_client::{
    GatewayConfig, GatewayConfigOverrides, GatewayEndpoint, DEFAULT_GATEWAY_SOCKET,
    SANDBOX_GATEWAY_AUTH_TOKEN_ENV, SANDBOX_GATEWAY_SOCKET_ENV,
};

#[test]
fn config_precedence_is_override_environment_default() {
    let default_config = GatewayConfig::discover_with(GatewayConfigOverrides::default(), |_| None)
        .expect("default config discovers");
    assert_eq!(
        default_config.gateway_socket_path,
        PathBuf::from(DEFAULT_GATEWAY_SOCKET)
    );
    assert_eq!(
        default_config.gateway_endpoint,
        GatewayEndpoint::parse(DEFAULT_GATEWAY_SOCKET).expect("default endpoint")
    );

    let env_config =
        GatewayConfig::discover_with(GatewayConfigOverrides::default(), |key| match key {
            SANDBOX_GATEWAY_SOCKET_ENV => Some(OsString::from("unix:///env/gateway.sock")),
            SANDBOX_GATEWAY_AUTH_TOKEN_ENV => Some(OsString::from("env-token")),
            _ => None,
        })
        .expect("environment config discovers");
    assert_eq!(
        env_config.gateway_socket_path,
        PathBuf::from("unix:///env/gateway.sock")
    );
    assert_eq!(env_config.gateway_auth_token.as_deref(), Some("env-token"));

    let override_config = GatewayConfig::discover_with(
        GatewayConfigOverrides {
            gateway_socket_path: Some(PathBuf::from("npipe://./pipe/override/gateway")),
            gateway_auth_token: Some("override-token".to_owned()),
        },
        |_| None,
    )
    .expect("overrides discover");
    assert_eq!(
        override_config.gateway_socket_path,
        PathBuf::from("npipe://./pipe/override/gateway")
    );
    assert_eq!(
        override_config.gateway_auth_token.as_deref(),
        Some("override-token")
    );
}

#[test]
fn config_rejects_empty_socket_and_blank_auth_tokens() {
    let socket_error = GatewayConfig::discover_with(
        GatewayConfigOverrides {
            gateway_socket_path: Some(PathBuf::new()),
            gateway_auth_token: None,
        },
        |_| None,
    )
    .expect_err("empty socket is rejected");
    assert_eq!(
        socket_error.to_string(),
        "gateway socket path must be non-empty"
    );

    for overrides in [
        GatewayConfigOverrides {
            gateway_socket_path: None,
            gateway_auth_token: Some(" ".to_owned()),
        },
        GatewayConfigOverrides::default(),
    ] {
        let error = GatewayConfig::discover_with(overrides, |key| {
            (key == SANDBOX_GATEWAY_AUTH_TOKEN_ENV).then(|| OsString::from(" "))
        })
        .expect_err("blank auth token is rejected");
        assert_eq!(error.to_string(), "gateway auth token must be non-empty");
    }
}
