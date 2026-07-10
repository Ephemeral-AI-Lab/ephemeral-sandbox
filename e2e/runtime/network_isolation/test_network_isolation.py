import platform
import subprocess
import textwrap
import time

from core.cli import internal_runtime, is_error, runtime
from core.config import IMAGE
from manager.management import helpers as mgmt


HELPER_SOURCE = r"""
use std::env;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("server") => server(args.get(2).expect("server label"), args.get(3)),
        Some("ip") => print_ip(),
        Some("get") => get(args.get(2).expect("host"), args.get(3)),
        _ => Err("usage: eos_http_probe server LABEL [SECONDS] | ip | get HOST [EXPECTED]".into()),
    };
    if let Err(error) = result {
        eprintln!("ERR {error}");
        std::process::exit(2);
    }
}

fn server(label: &str, seconds: Option<&String>) -> Result<(), String> {
    let lifetime = seconds.and_then(|value| value.parse::<u64>().ok()).unwrap_or(30);
    let deadline = Instant::now() + Duration::from_secs(lifetime);
    let listener = TcpListener::bind("0.0.0.0:3000").map_err(|err| err.to_string())?;
    listener.set_nonblocking(true).map_err(|err| err.to_string())?;
    println!("LISTEN {label}");
    std::io::stdout().flush().map_err(|err| err.to_string())?;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf);
                let body = format!("HTTP_BODY:{label}\n");
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).map_err(|err| err.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn print_ip() -> Result<(), String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|err| err.to_string())?;
    socket.connect("10.244.0.1:9").map_err(|err| err.to_string())?;
    println!("{}", socket.local_addr().map_err(|err| err.to_string())?.ip());
    Ok(())
}

fn get(host: &str, expected: Option<&String>) -> Result<(), String> {
    let addr: SocketAddr = format!("{host}:3000").parse::<SocketAddr>().map_err(|err| err.to_string())?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(700))
        .map_err(|err| err.to_string())?;
    stream.set_read_timeout(Some(Duration::from_millis(700))).map_err(|err| err.to_string())?;
    stream.write_all(b"GET / HTTP/1.1\r\nhost: probe\r\nconnection: close\r\n\r\n")
        .map_err(|err| err.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|err| err.to_string())?;
    print!("{response}");
    if let Some(expected) = expected {
        if !response.contains(expected) {
            return Err(format!("response did not contain {expected:?}"));
        }
    }
    Ok(())
}
"""


def test_isolated_workspace_sessions_cannot_reach_each_other_on_same_port(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    helper = compile_helper(workspace)
    sandbox_id = None
    workspace_ids = []
    command_ids = []
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"

        for _ in range(3):
            result = internal_runtime(
                sandbox_id,
                "create_workspace_session",
                {"network_profile": "isolated"},
            )
            assert not is_error(result), result
            workspace_ids.append(result["workspace_session_id"])

        labels = [f"ws{i}" for i in range(1, 4)]
        for workspace_id, label in zip(workspace_ids, labels):
            result = exec_in(
                sandbox_id,
                workspace_id,
                f"./{helper.name} server {label} 30",
                yield_ms=250,
                timeout_ms=30_000,
            )
            assert result["status"] == "running", result
            assert f"LISTEN {label}" in result["output"], result
            command_ids.append(result["command_session_id"])

        ips = []
        for workspace_id in workspace_ids:
            result = exec_in(sandbox_id, workspace_id, f"./{helper.name} ip")
            assert result["status"] == "ok", result
            ips.append(result["output"].strip())

        assert len(set(ips)) == 3

        for workspace_id, label in zip(workspace_ids, labels):
            result = exec_in(sandbox_id, workspace_id, f"./{helper.name} get 127.0.0.1 {label}")
            assert result["status"] == "ok", result
            assert f"HTTP_BODY:{label}" in result["output"], result

        for source_index, workspace_id in enumerate(workspace_ids):
            for target_index, target_ip in enumerate(ips):
                if source_index == target_index:
                    continue
                result = exec_in(
                    sandbox_id,
                    workspace_id,
                    f"./{helper.name} get {target_ip} {labels[target_index]}",
                    timeout=20,
                )
                assert result["status"] != "ok", {
                    "source_workspace": workspace_id,
                    "target_ip": target_ip,
                    "result": result,
                }
    finally:
        if sandbox_id:
            stop_commands(sandbox_id, command_ids)
            for workspace_id in workspace_ids:
                internal_runtime(
                    sandbox_id,
                    "destroy_workspace_session",
                    {"workspace_session_id": workspace_id, "grace_s": 1},
                )
            mgmt.destroy_sandbox(sandbox_id)


def compile_helper(workspace):
    source = workspace / "eos_http_probe.rs"
    binary = workspace / "eos_http_probe"
    source.write_text(textwrap.dedent(HELPER_SOURCE).strip() + "\n")
    subprocess.run(
        [
            "rustc",
            "--edition",
            "2021",
            "--target",
            linux_musl_target(),
            "-C",
            "linker=rust-lld",
            "-O",
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    return binary


def linux_musl_target():
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "aarch64-unknown-linux-musl"
    if machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-musl"
    raise AssertionError(f"unsupported test host architecture: {machine}")


def exec_in(
    sandbox_id,
    workspace_id,
    command,
    *,
    yield_ms=1_000,
    timeout_ms=None,
    timeout=180,
):
    args = ["--workspace-session-id", workspace_id]
    if timeout_ms is not None:
        args += ["--timeout-ms", str(timeout_ms)]
    if yield_ms is not None:
        args += ["--yield-time-ms", str(yield_ms)]
    args.append(command)
    return runtime(sandbox_id, "exec_command", *args, timeout=timeout)


def stop_commands(sandbox_id, command_ids):
    live = set(command_ids)
    for command_id in list(live):
        runtime(
            sandbox_id,
            "write_command_stdin",
            "--command-session-id",
            command_id,
            "--yield-time-ms",
            "500",
            "\u0003",
            timeout=10,
        )

    deadline = time.monotonic() + 35
    while live and time.monotonic() < deadline:
        for command_id in list(live):
            result = runtime(
                sandbox_id,
                "read_command_lines",
                "--command-session-id",
                command_id,
                "--start-offset",
                "0",
                "--limit",
                "5",
                timeout=10,
            )
            if result.get("status") != "running":
                live.remove(command_id)
        if live:
            time.sleep(0.5)
