"""Live Docker E2E for the daemon HTTP surface (`daemon_http`).

Drives real sandboxes through ``sandbox-cli manager`` and real in-sandbox HTTP
servers through ``sandbox-cli runtime exec_command``, then calls the published
``daemon_http`` loopback port from the host:

  * ``/health`` returns the fixed status document.
  * ``/forward/shared/<port>/...`` reaches a server in the shared network.
  * ``/forward/isolated=<workspace_id>/<port>/...`` reaches a server bound to
    ``0.0.0.0`` inside an isolated workspace session.
  * ``POST /files/list`` sees published and live-session directory state.
  * Removed file, observability, and export operation routes return ``404``.
  * invalid routes map to ``400``/``404``.

Each success test binds port ``0`` inside the sandbox and forwards to the
assigned port — never a hardcoded one. Every sandbox, workspace session, and
running command is cleaned up.
"""

import json
import platform
import re
import subprocess
import textwrap
import time
import urllib.error
import urllib.request

from core.cli import internal_runtime, is_error, runtime
from core.config import IMAGE
from manager.management import helpers as mgmt


# A tiny static HTTP probe: bind 0.0.0.0:0, print the assigned port, then echo
# the route/workspace plus the received path and query in each response body.
# Binding 0.0.0.0 keeps it reachable both as 127.0.0.1 (shared network) and via
# the workspace IP (isolated network).
HELPER_SOURCE = r"""
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = env::args().collect();
    let route = args.get(1).cloned().unwrap_or_default();
    let workspace = match args.get(2).map(String::as_str) {
        Some("-") | None => None,
        Some(value) => Some(value.to_owned()),
    };
    let lifetime = args.get(3).and_then(|value| value.parse::<u64>().ok()).unwrap_or(60);
    if let Err(error) = serve(&route, workspace.as_deref(), lifetime) {
        eprintln!("ERR {error}");
        std::process::exit(2);
    }
}

fn serve(route: &str, workspace: Option<&str>, lifetime: u64) -> Result<(), String> {
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    listener.set_nonblocking(true).map_err(|err| err.to_string())?;
    println!("PORT={port}");
    std::io::stdout().flush().map_err(|err| err.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(lifetime);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                let mut buf = [0_u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (path, query) = match target.split_once('?') {
                    Some((path, query)) => (path, query),
                    None => (target, ""),
                };
                let mut body = format!("route={route}\n");
                if let Some(workspace) = workspace {
                    body.push_str(&format!("workspace={workspace}\n"));
                }
                body.push_str(&format!("path={path}\n"));
                body.push_str(&format!("query={query}\n"));
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}
"""


def test_daemon_http_health(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    sandbox_id = None
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"

        host, port = daemon_http_endpoint(created, sandbox_id)
        status, body, content_type = http_get(f"http://{host}:{port}/health")

        assert status == 200, body
        assert "application/json" in content_type, content_type
        document = json.loads(body)
        assert document["status"] == "ok", document
        assert document["service"] == "daemon_http", document
    finally:
        if sandbox_id:
            mgmt.destroy_sandbox(sandbox_id)


def test_forward_shared_arbitrary_port(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    helper = compile_helper(workspace)
    sandbox_id = None
    command_id = None
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"
        host, daemon_port = daemon_http_endpoint(created, sandbox_id)

        # No workspace session: exec_command runs in a one-shot shared-network
        # workspace, so the server is reachable on the daemon's 127.0.0.1.
        result = runtime(
            sandbox_id,
            "exec_command",
            "--yield-time-ms",
            "500",
            "--timeout-ms",
            "60000",
            f"./{helper.name} shared - 60",
        )
        assert result["status"] == "running", result
        command_id = result["command_session_id"]
        server_port = read_port(result["output"])

        status, body, _ = http_get(
            f"http://{host}:{daemon_port}/forward/shared/{server_port}/nested/path?hello=world"
        )

        assert status == 200, body
        assert "route=shared" in body, body
        assert "path=/nested/path" in body, body
        assert "query=hello=world" in body, body
    finally:
        if sandbox_id:
            stop_command(sandbox_id, command_id)
            mgmt.destroy_sandbox(sandbox_id)


def test_forward_isolated_arbitrary_port(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    helper = compile_helper(workspace)
    sandbox_id = None
    workspace_id = None
    command_id = None
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"
        host, daemon_port = daemon_http_endpoint(created, sandbox_id)

        session = internal_runtime(
            sandbox_id,
            "create_workspace_session",
            {"network_profile": "isolated"},
        )
        assert not is_error(session), session
        workspace_id = session["workspace_session_id"]

        result = runtime(
            sandbox_id,
            "exec_command",
            "--workspace-session-id",
            workspace_id,
            "--yield-time-ms",
            "500",
            "--timeout-ms",
            "60000",
            f"./{helper.name} isolated {workspace_id} 60",
        )
        assert result["status"] == "running", result
        command_id = result["command_session_id"]
        server_port = read_port(result["output"])

        status, body, _ = http_get(
            f"http://{host}:{daemon_port}"
            f"/forward/isolated={workspace_id}/{server_port}/nested/path?hello=isolated"
        )

        assert status == 200, body
        assert "route=isolated" in body, body
        assert f"workspace={workspace_id}" in body, body
        assert "path=/nested/path" in body, body
        assert "query=hello=isolated" in body, body
    finally:
        if sandbox_id:
            stop_command(sandbox_id, command_id)
            if workspace_id:
                internal_runtime(
                    sandbox_id,
                    "destroy_workspace_session",
                    {"workspace_session_id": workspace_id, "grace_s": 1},
                )
            mgmt.destroy_sandbox(sandbox_id)


def test_forward_rejects_invalid_routes(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    sandbox_id = None
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"
        host, port = daemon_http_endpoint(created, sandbox_id)
        base = f"http://{host}:{port}"

        cases = {
            "/forward/shared/not-a-port/": 400,
            "/forward/shared/0/": 400,
            "/forward/isolated=missing/3000/": 404,
            "/not-forward/shared/3000/": 404,
        }
        for path, expected in cases.items():
            status, body, _ = http_get(base + path)
            assert status == expected, f"{path} -> {status} (expected {expected}): {body}"
    finally:
        if sandbox_id:
            mgmt.destroy_sandbox(sandbox_id)


def test_file_list_and_removed_operation_routes(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "published.txt").write_text("published\n", encoding="utf-8")
    sandbox_id = None
    workspace_id = None
    try:
        created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
        sandbox_id = created.get("id")
        assert sandbox_id, f"create_sandbox failed: {created}"
        host, port = daemon_http_endpoint(created, sandbox_id)
        base = f"http://{host}:{port}"

        status, body, content_type = http_post(base + "/files/list", {})
        assert status == 200, body
        assert "application/json" in content_type, content_type
        root = json.loads(body)
        assert not is_error(root), root
        assert "published.txt" in entry_names(root), root

        session = internal_runtime(sandbox_id, "create_workspace_session", {})
        assert not is_error(session), session
        workspace_id = session["workspace_session_id"]
        written = runtime(
            sandbox_id,
            "file_write",
            "--path",
            "live.txt",
            "--content",
            "live\n",
            "--workspace-session-id",
            workspace_id,
        )
        assert not is_error(written), written

        status, body, _ = http_post(
            base + "/files/list",
            {"workspace_session_id": workspace_id},
        )
        assert status == 200, body
        live = json.loads(body)
        assert not is_error(live), live
        names = entry_names(live)
        assert "published.txt" in names, live
        assert "live.txt" in names, live

        removed = [
            "/files/read",
            "/files/write",
            "/files/edit",
            "/files/blame",
            "/observability/snapshot",
            "/observability/trace",
            "/observability/events",
            "/observability/cgroup",
            "/observability/layerstack",
            "/export/legacy",
            "/files/list/extra",
        ]
        for path in removed:
            status, body, _ = http_post(base + path, {})
            assert status == 404, f"POST {path} -> {status}: {body}"
    finally:
        if sandbox_id:
            if workspace_id:
                internal_runtime(
                    sandbox_id,
                    "destroy_workspace_session",
                    {"workspace_session_id": workspace_id, "grace_s": 1},
                )
            mgmt.destroy_sandbox(sandbox_id)


def daemon_http_endpoint(created, sandbox_id):
    endpoint = created.get("daemon_http")
    if not endpoint:
        inspected = mgmt.inspect_sandbox(sandbox_id)
        endpoint = inspected.get("daemon_http")
    assert endpoint, f"record is missing daemon_http endpoint: {created}"
    return endpoint["host"], int(endpoint["port"])


def http_get(url, attempts=20):
    return http_request(url, attempts=attempts)


def http_post(url, document, attempts=20):
    return http_request(
        url,
        method="POST",
        body=json.dumps(document).encode("utf-8"),
        attempts=attempts,
    )


def http_request(url, method="GET", body=None, attempts=20):
    last_error = None
    for _ in range(attempts):
        try:
            request = urllib.request.Request(
                url,
                data=body,
                headers={"Content-Type": "application/json"} if body is not None else {},
                method=method,
            )
            with urllib.request.urlopen(request, timeout=10) as response:
                return (
                    response.status,
                    response.read().decode("utf-8", "replace"),
                    response.headers.get_content_type(),
                )
        except urllib.error.HTTPError as error:
            content_type = error.headers.get_content_type() if error.headers else ""
            return error.code, error.read().decode("utf-8", "replace"), content_type
        except urllib.error.URLError as error:
            last_error = error
            time.sleep(0.25)
    raise AssertionError(f"{method} {url} never connected: {last_error}")


def entry_names(document):
    return {entry["name"] for entry in document.get("entries", [])}


def read_port(output):
    match = re.search(r"PORT=(\d+)", output)
    assert match, f"server did not print its assigned port: {output!r}"
    return int(match.group(1))


def stop_command(sandbox_id, command_id):
    if not command_id:
        return
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


def compile_helper(workspace):
    source = workspace / "eos_daemon_http_probe.rs"
    binary = workspace / "eos_daemon_http_probe"
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
