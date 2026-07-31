use std::path::PathBuf;

#[test]
fn gateway_server_config_constructs_from_defaults() {
    let config = GatewayConfig::new(
        DEFAULT_GATEWAY_SOCKET,
        DEFAULT_GATEWAY_PID,
        DEFAULT_MAX_CONCURRENT_CONNECTIONS,
        None,
    );

    assert_eq!(config.bind_addr, DEFAULT_GATEWAY_SOCKET);
    assert_eq!(config.pid_path, PathBuf::from(DEFAULT_GATEWAY_PID));
    assert_eq!(
        config.max_concurrent_connections,
        DEFAULT_MAX_CONCURRENT_CONNECTIONS
    );
    assert!(config.auth_token.is_none());
}

#[test]
fn gateway_endpoints_parse_supported_transports_and_legacy_tcp() {
    let endpoints = [
        (
            "127.0.0.1:7878",
            GatewayEndpoint::Tcp("127.0.0.1:7878".parse().expect("socket address")),
        ),
        (
            "tcp://[::1]:7878",
            GatewayEndpoint::Tcp("[::1]:7878".parse().expect("socket address")),
        ),
        (
            "npipe://./pipe/ephemeral-sandbox/user-1",
            GatewayEndpoint::WindowsNamedPipe(
                r"\\.\pipe\ephemeral-sandbox\user-1".to_owned(),
            ),
        ),
        (
            "unix:///tmp/ephemeral-sandbox.sock",
            GatewayEndpoint::UnixSocket(PathBuf::from("/tmp/ephemeral-sandbox.sock")),
        ),
    ];

    for (raw, expected) in endpoints {
        let parsed = raw
            .parse::<GatewayEndpoint>()
            .expect("supported endpoint parses");
        assert_eq!(parsed, expected);
        assert_eq!(
            parsed.to_string(),
            match raw {
                "127.0.0.1:7878" => "tcp://127.0.0.1:7878",
                _ => raw,
            }
        );
    }
}

#[test]
fn gateway_endpoints_reject_unsafe_or_ambiguous_values() {
    let invalid = [
        "",
        " 127.0.0.1:7878",
        "127.0.0.1:7878 ",
        "http://127.0.0.1:7878",
        "tcp://localhost:7878",
        "tcp://127.0.0.1:0",
        "npipe://server/pipe/name",
        "npipe://./pipe/",
        "npipe://./pipe/../escape",
        "npipe://./pipe/name\\escape",
        "unix://relative.sock",
        "unix:////tmp/ambiguous.sock",
        "unix:///tmp/../escape.sock",
        "unix:///tmp//ambiguous.sock",
    ];

    for value in invalid {
        let error = value
            .parse::<GatewayEndpoint>()
            .expect_err("unsafe endpoint must fail");
        assert!(error.to_string().contains("invalid gateway endpoint"));
    }
}

#[test]
fn config_gateway_defaults_preserve_shipped_policy() {
    // prd.yml carries no gateway section, so the section must load to
    // today's exact constants.
    let config = GatewayConfig::default();
    config.validate().expect("default gateway config is valid");
    assert_eq!(config.bind_addr, DEFAULT_GATEWAY_SOCKET);
    assert_eq!(config.pid_path, PathBuf::from("/tmp/eos-gateway.pid"));
    assert_eq!(config.max_concurrent_connections, 256);
    assert!(config.auth_token.is_none());
}

#[test]
fn config_gateway_section_overrides_deserialize() {
    let config = gateway_section(
        "gateway:
  bind_addr: 127.0.0.1:7912
  pid_path: /tmp/alt-gateway.pid
  max_concurrent_connections: 4
",
    )
    .expect("gateway overrides deserialize");
    config.validate().expect("gateway overrides are valid");
    assert_eq!(config.bind_addr, "127.0.0.1:7912");
    assert_eq!(config.pid_path, PathBuf::from("/tmp/alt-gateway.pid"));
    assert_eq!(config.max_concurrent_connections, 4);
    assert!(config.auth_token.is_none());
}

#[test]
fn config_gateway_partial_section_fills_defaults() {
    let config = gateway_section("gateway:\n  max_concurrent_connections: 4\n")
        .expect("partial gateway section deserializes");
    assert_eq!(config.bind_addr, DEFAULT_GATEWAY_SOCKET);
    assert_eq!(config.max_concurrent_connections, 4);
}

#[test]
fn config_gateway_rejects_unknown_key() {
    let error = gateway_section("gateway:\n  socket: 127.0.0.1:1\n")
        .expect_err("unknown gateway key must be rejected");
    assert!(error.to_string().contains("socket"), "{error}");
}

#[test]
fn config_gateway_rejects_auth_token_in_yaml() {
    // The auth token is runtime state (flag/env only); a YAML key must fail
    // loudly rather than silently feed a secret through config.
    let error = gateway_section("gateway:\n  auth_token: sneaky\n")
        .expect_err("auth_token in YAML must be rejected");
    assert!(error.to_string().contains("auth_token"), "{error}");
}

#[test]
fn config_validation_rejects_gateway_edge_values() {
    let invalid = [
        (
            GatewayConfig {
                bind_addr: String::new(),
                ..GatewayConfig::default()
            },
            "gateway.bind_addr",
        ),
        (
            GatewayConfig {
                bind_addr: "not-an-address".to_owned(),
                ..GatewayConfig::default()
            },
            "gateway.bind_addr",
        ),
        (
            GatewayConfig {
                bind_addr: "npipe://./pipe/../escape".to_owned(),
                ..GatewayConfig::default()
            },
            "gateway.bind_addr",
        ),
        (
            GatewayConfig {
                bind_addr: "unix://relative.sock".to_owned(),
                ..GatewayConfig::default()
            },
            "gateway.bind_addr",
        ),
        (
            GatewayConfig {
                pid_path: PathBuf::new(),
                ..GatewayConfig::default()
            },
            "gateway.pid_path",
        ),
        (
            GatewayConfig {
                max_concurrent_connections: 0,
                ..GatewayConfig::default()
            },
            "gateway.max_concurrent_connections",
        ),
    ];
    for (config, field) in &invalid {
        assert_invalid(config, field);
    }
}

fn gateway_section(yaml: &str) -> Result<GatewayConfig, crate::ConfigError> {
    crate::ConfigDocument::parse(std::path::Path::new("<test>"), yaml)?.section("gateway")
}

fn assert_invalid(config: &GatewayConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    assert!(err.to_string().contains(field), "{err}");
}
