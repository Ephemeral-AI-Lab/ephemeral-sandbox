use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use sandbox_operation_client::{GatewayEndpoint, GatewayEndpointParseError};

#[test]
fn parses_explicit_and_legacy_tcp_endpoints() {
    let ipv4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 7878));
    let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 7878));

    assert_eq!(
        GatewayEndpoint::parse("tcp://127.0.0.1:7878").expect("explicit IPv4"),
        GatewayEndpoint::Tcp(ipv4)
    );
    assert_eq!(
        GatewayEndpoint::parse("127.0.0.1:7878").expect("legacy IPv4"),
        GatewayEndpoint::Tcp(ipv4)
    );
    assert_eq!(
        GatewayEndpoint::parse("tcp://[::1]:7878").expect("explicit IPv6"),
        GatewayEndpoint::Tcp(ipv6)
    );
}

#[test]
fn parses_local_ipc_endpoints_to_native_paths() {
    let named_pipe =
        GatewayEndpoint::parse("npipe://./pipe/ephemeral-sandbox/gateway").expect("named pipe");
    let unix_socket = GatewayEndpoint::parse("unix:///run/user/1000/ephemeral-sandbox.sock")
        .expect("Unix socket");

    assert_eq!(
        named_pipe,
        GatewayEndpoint::WindowsNamedPipe(r"\\.\pipe\ephemeral-sandbox\gateway".to_owned())
    );
    assert_eq!(
        unix_socket,
        GatewayEndpoint::UnixSocket(PathBuf::from("/run/user/1000/ephemeral-sandbox.sock"))
    );
    assert_eq!(
        named_pipe.to_string(),
        "npipe://./pipe/ephemeral-sandbox/gateway"
    );
    assert_eq!(
        unix_socket.to_string(),
        "unix:///run/user/1000/ephemeral-sandbox.sock"
    );
}

#[test]
fn endpoint_constructors_validate_local_paths() {
    assert_eq!(
        GatewayEndpoint::windows_named_pipe(r"\\.\pipe\gateway").expect("native pipe"),
        GatewayEndpoint::WindowsNamedPipe(r"\\.\pipe\gateway".to_owned())
    );
    assert_eq!(
        GatewayEndpoint::unix_socket("/tmp/gateway.sock").expect("absolute Unix path"),
        GatewayEndpoint::UnixSocket(PathBuf::from("/tmp/gateway.sock"))
    );
    assert!(GatewayEndpoint::windows_named_pipe(r"\\.\pipe\gateway/name").is_err());
}

#[test]
fn rejects_invalid_endpoint_syntax() {
    let cases = [
        ("", "gateway endpoint must be non-empty"),
        (
            "http://127.0.0.1:7878",
            "unsupported gateway endpoint scheme",
        ),
        (
            "tcp://localhost:7878",
            "TCP gateway endpoint must be a valid IP socket address",
        ),
        (
            "tcp://127.0.0.1:0",
            "TCP gateway endpoint port must be non-zero",
        ),
        (
            " tcp://127.0.0.1:7878",
            "gateway endpoint must not contain surrounding whitespace",
        ),
        (
            "npipe://./pipe/gateway/../other",
            "Windows named-pipe path must not contain dot segments",
        ),
        (
            "npipe://./pipe/gateway:other",
            "Windows named-pipe name contains an unsafe character",
        ),
        (
            r"npipe://./pipe/gateway\other",
            "Windows named-pipe name contains an unsafe character",
        ),
        (
            "unix://relative.sock",
            "Unix-domain socket path must be absolute",
        ),
        (
            "unix:///tmp//gateway.sock",
            "Unix-domain socket path must not contain empty segments",
        ),
        (
            "unix:///tmp/gateway\\other.sock",
            "Unix-domain socket path contains an unsafe character",
        ),
    ];

    for (value, expected) in cases {
        let error: GatewayEndpointParseError =
            GatewayEndpoint::parse(value).expect_err("endpoint is invalid");
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn rejects_local_ipc_paths_over_the_transport_limits() {
    let pipe_name = "a".repeat(250);
    let pipe = format!("npipe://./pipe/{pipe_name}");
    assert_eq!(
        GatewayEndpoint::parse(&pipe)
            .expect_err("named-pipe path is too long")
            .to_string(),
        "Windows named-pipe path exceeds the 256-character limit"
    );

    let socket_name = "a".repeat(96);
    let socket = format!("unix:///tmp/{socket_name}");
    assert_eq!(
        GatewayEndpoint::parse(&socket)
            .expect_err("Unix-domain socket path is too long")
            .to_string(),
        "Unix-domain socket path exceeds the portable 100-byte limit"
    );
}
