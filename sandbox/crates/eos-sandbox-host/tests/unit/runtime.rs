use std::path::PathBuf;

use super::{
    docker_exec_args, docker_http_status, docker_unix_socket_from_host, parse_published_addr,
    percent_encode, tar_single_file,
};

#[test]
fn docker_helpers_parse_http_and_unix_host() {
    assert_eq!(
        docker_http_status("HTTP/1.1 200 OK\r\n\r\n").expect("status"),
        200
    );
    assert_eq!(
        percent_encode("/eos/runtime/daemon"),
        "%2Feos%2Fruntime%2Fdaemon"
    );
    assert_eq!(
        docker_unix_socket_from_host("unix:///var/run/docker.sock").expect("socket"),
        PathBuf::from("/var/run/docker.sock")
    );
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
}

#[test]
fn published_addr_parses_loopback_port() {
    assert_eq!(
        parse_published_addr("0.0.0.0:54321"),
        Some("127.0.0.1:54321".parse().expect("addr"))
    );
    assert_eq!(parse_published_addr("garbage"), None);
}

#[test]
fn tar_single_file_builds_executable_ustar_stream() {
    let tar = tar_single_file("eosd", b"payload", 0o755).expect("tar stream");
    assert_eq!(&tar[0..4], b"eosd");
    assert_eq!(&tar[100..108], b"0000755\0");
    assert_eq!(&tar[124..136], b"00000000007\0");
    assert_eq!(tar[156], b'0');
    assert_eq!(&tar[257..263], b"ustar\0");
    assert_eq!(tar.len() % 512, 0);
}
