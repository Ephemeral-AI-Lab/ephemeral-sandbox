//! Isolated-workspace network isolation (spec point 3).
//!
//! An isolated session runs in its OWN network namespace (veth + net fd), while
//! ephemeral execs share the container netns. So an ephemeral server and an
//! isolated server can bind the SAME port with no conflict, whereas two ephemeral
//! servers on the same port collide (EADDRINUSE).
//!
//! Robustness: each test picks a UNIQUE port (cross-run collisions impossible),
//! servers are bounded by `timeout` (a leak self-heals fast), assertions use
//! `ensure!` so the cleanup that cancels servers / exits the isolated session
//! runs even on failure, and the isolated session for caller B is always exited.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{as_str, live_pool_or_skip, reset_isolated_workspaces, stdout};

/// A high, per-run-unique port so a server that outlives one test (e.g. its
/// `timeout` window) never collides with the same fixed port in the next run.
fn unique_port() -> u16 {
    let hash = eos_e2e_test::unique_suffix()
        .bytes()
        .fold(0_u32, |acc, byte| {
            acc.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
    40_000 + u16::try_from(hash % 8_000).unwrap_or(0)
}

fn start_server(
    lease: &eos_e2e_test::NodeLease<'_>,
    caller_id: Option<&str>,
    port: u16,
) -> Result<Value> {
    // Log to /tmp (writable in the ephemeral exec): /eos is read-only by the
    // mount mask, so a `>/eos/scratch/...` redirect makes the server fail before
    // it binds the port — which would make the conflict checks pass for the
    // wrong reason. The log itself is throwaway (never read by any test).
    let cmd = format!("timeout 20 python3 -m http.server {port} >/tmp/eos-e2e-srv-{port}.log 2>&1");
    let mut args = json!({
        "cmd": cmd,
        "yield_time_ms": 400,
        "timeout_seconds": 60,});
    if let Some(caller) = caller_id {
        args["caller_id"] = json!(caller);
    }
    lease.call_ok(ops::API_V1_EXEC_COMMAND, args)
}

fn start_bound_socket(
    lease: &eos_e2e_test::NodeLease<'_>,
    caller_id: &str,
    port: u16,
    label: &str,
) -> Result<Value> {
    lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "caller_id": caller_id,
            "cmd": format!("python3 -c \"import socket,time;s=socket.socket();s.bind(('0.0.0.0',{port}));s.listen(1);print('BOUND-{label}',flush=True);time.sleep(20)\""),
            "yield_time_ms": 600,
            "timeout_seconds": 60,}),
    )
}

fn cancel(lease: &eos_e2e_test::NodeLease<'_>, caller_id: Option<&str>, id: &str) {
    let mut args = json!({"command_session_id": id});
    if let Some(caller) = caller_id {
        args["caller_id"] = json!(caller);
    }
    let _ = lease.call(ops::API_V1_COMMAND_CANCEL, args);
}

fn wait_for_output(
    lease: &eos_e2e_test::NodeLease<'_>,
    caller_id: Option<&str>,
    response: &Value,
    marker: &str,
) -> Result<()> {
    if stdout(response).contains(marker) {
        return Ok(());
    }
    let session_id = as_str(response, "command_session_id")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = response.clone();
    loop {
        let mut poll_args = json!({
            "command_session_id": session_id,
            "last_n_lines": 8,
        });
        if let Some(caller) = caller_id {
            poll_args["caller_id"] = json!(caller);
        }
        if let Ok(poll) = lease.call_ok(ops::API_V1_COMMAND_READ_PROGRESS, poll_args) {
            if stdout(&poll).contains(marker) {
                return Ok(());
            }
            last = poll;
        }

        let mut collect_args = json!({"command_session_ids": [session_id]});
        if let Some(caller) = caller_id {
            collect_args["caller_id"] = json!(caller);
        }
        let collected = lease.call_ok(ops::API_V1_COMMAND_COLLECT_COMPLETED, collect_args)?;
        let completions = collected
            .get("completions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(completion) = completions.first() {
            let result = completion
                .get("result")
                .context("command session completion result")?;
            if stdout(result).contains(marker) {
                return Ok(());
            }
            last = result.clone();
        }

        if Instant::now() >= deadline {
            bail!("session output never contained {marker}: {last}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn bind_probe_command(port: u16, success_marker: &str) -> String {
    format!(
        "python3 - <<'PY'\nimport socket\ns = socket.socket()\ntry:\n    s.bind(('0.0.0.0', {port}))\nexcept OSError:\n    print('EADDRINUSE', flush=True)\nelse:\n    print('{success_marker}', flush=True)\nPY"
    )
}

fn loopback_server_command(port: u16, marker: &str) -> String {
    format!(
        r#"python3 -u - <<'PY'
import socket
import time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", {port}))
s.listen(5)
print("SERVER_READY", flush=True)
deadline = time.time() + 20
while time.time() < deadline:
    s.settimeout(1)
    try:
        conn, _ = s.accept()
    except TimeoutError:
        continue
    with conn:
        conn.sendall({marker:?}.encode())
PY"#
    )
}

fn loopback_probe_command(port: u16, success_marker: &str) -> String {
    format!(
        r#"python3 - <<'PY'
import socket
s = socket.socket()
s.settimeout(1)
try:
    s.connect(("127.0.0.1", {port}))
    data = s.recv(128).decode("utf-8", "replace")
except OSError:
    print("PEER_BLOCKED", flush=True)
else:
    print(data or "{success_marker}", flush=True)
PY"#
    )
}

#[test]
fn cross_mode_same_port_no_conflict() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let port = unique_port();
    let caller_b = format!("iws-net-b-{}", eos_e2e_test::unique_suffix());

    // Caller A: an ephemeral server holding the port in the container netns.
    let server_a = start_server(&lease, None, port)?;
    let id_a = as_str(&server_a, "command_session_id")
        .ok()
        .map(ToOwned::to_owned);

    let body = (|| -> Result<()> {
        ensure!(
            as_str(&server_a, "status")? == "running",
            "ephemeral server must start: {server_a}"
        );
        // Caller B enters isolated mode (its own netns), then binds the SAME port.
        lease.call_ok(
            ops::API_ISOLATED_WORKSPACE_ENTER,
            json!({"caller_id": caller_b}),
        )?;
        let bind_b = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "caller_id": caller_b,
                "cmd": format!("python3 -c \"import socket,time;s=socket.socket();s.bind(('0.0.0.0',{port}));print('BOUND',flush=True);time.sleep(20)\""),
                "yield_time_ms": 600,
                "timeout_seconds": 60,}),
        )?;
        ensure!(
            as_str(&bind_b, "status")? == "running",
            "isolated server must stay running: {bind_b}"
        );
        wait_for_output(&lease, Some(&caller_b), &bind_b, "BOUND")
            .with_context(|| format!("isolated server must bind the same port: {bind_b}"))?;
        if let Some(id_b) = bind_b.get("command_session_id").and_then(Value::as_str) {
            cancel(&lease, Some(&caller_b), id_b);
        }
        Ok(())
    })();

    if let Some(id_a) = id_a.as_deref() {
        cancel(&lease, None, id_a);
    }
    let _ = lease.call(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_b, "grace_s": 0.1}),
    );
    body
}

#[test]
fn same_mode_same_port_conflicts() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let port = unique_port();

    // Two ephemeral execs share the container netns: the second bind collides.
    let server = start_server(&lease, None, port)?;
    let id = as_str(&server, "command_session_id")
        .ok()
        .map(ToOwned::to_owned);

    let body = (|| -> Result<()> {
        ensure!(
            as_str(&server, "status")? == "running",
            "first ephemeral server must start: {server}"
        );
        let bind = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "cmd": bind_probe_command(port, "BOUND"),
                "yield_time_ms": 2000,
                "timeout_seconds": 30,}),
        )?;
        wait_for_output(&lease, None, &bind, "EADDRINUSE").with_context(|| {
            format!("a second ephemeral bind on the same port must fail: {bind}")
        })?;
        ensure!(
            !stdout(&bind).contains("BOUND"),
            "the second ephemeral bind must not succeed: {bind}"
        );
        Ok(())
    })();

    if let Some(id) = id.as_deref() {
        cancel(&lease, None, id);
    }
    body
}

#[test]
fn isolated_loopback_service_is_not_reachable_from_peer_session() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let port = unique_port();
    let caller_a = format!("iws-loopback-a-{}", eos_e2e_test::unique_suffix());
    let caller_b = format!("iws-loopback-b-{}", eos_e2e_test::unique_suffix());
    let marker = format!("A_ONLY_{}", eos_e2e_test::unique_suffix().replace('-', "_"));

    lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller_a}),
    )?;
    lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller_b}),
    )?;

    let mut sessions: Vec<(String, String)> = Vec::new();
    let body = (|| -> Result<()> {
        let server_a = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "caller_id": caller_a,
                "cmd": loopback_server_command(port, &marker),
                "yield_time_ms": 600,
                "timeout_seconds": 60,}),
        )?;
        ensure!(
            as_str(&server_a, "status")? == "running",
            "caller A loopback server must stay running: {server_a}"
        );
        wait_for_output(&lease, Some(&caller_a), &server_a, "SERVER_READY")?;
        sessions.push((
            caller_a.clone(),
            as_str(&server_a, "command_session_id")?.to_owned(),
        ));

        let own_probe = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "caller_id": caller_a,
                "cmd": loopback_probe_command(port, &marker),
                "yield_time_ms": 2000,
                "timeout_seconds": 10,}),
        )?;
        wait_for_output(&lease, Some(&caller_a), &own_probe, &marker).with_context(|| {
            format!("caller A should reach its own loopback service: {own_probe}")
        })?;

        let peer_probe = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "caller_id": caller_b,
                "cmd": loopback_probe_command(port, &marker),
                "yield_time_ms": 2000,
                "timeout_seconds": 10,}),
        )?;
        wait_for_output(&lease, Some(&caller_b), &peer_probe, "PEER_BLOCKED").with_context(
            || format!("caller B must not reach caller A's loopback service: {peer_probe}"),
        )?;
        ensure!(
            !stdout(&peer_probe).contains(&marker),
            "peer session must not receive caller A's loopback marker: {peer_probe}"
        );
        Ok(())
    })();

    for (caller_id, session_id) in &sessions {
        cancel(&lease, Some(caller_id), session_id);
    }
    let _ = lease.call(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_b, "grace_s": 0.1}),
    );
    let _ = lease.call(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_a, "grace_s": 0.1}),
    );
    body
}

#[test]
fn isolated_exit_reports_dedicated_netns() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    let inspection = exit.get("inspection").context("exit inspection")?;
    // Four namespace fds (user, mnt, pid, net) — the net fd is the netns proof.
    assert_eq!(
        inspection.get("ns_fd_count").and_then(Value::as_i64),
        Some(4),
        "isolated session must hold its own net namespace fd: {exit}"
    );
    let veth_host = inspection
        .get("veth_host_name")
        .and_then(Value::as_str)
        .context("veth_host_name")?;
    let veth_ns = inspection
        .get("veth_ns_name")
        .and_then(Value::as_str)
        .context("veth_ns_name")?;
    assert!(
        veth_host.starts_with("eos-iws-") && veth_host.ends_with('h'),
        "host veth name should follow the eos-iws-*h convention: {veth_host}"
    );
    assert!(
        veth_ns.starts_with("eos-iws-") && veth_ns.ends_with('n'),
        "ns veth name should follow the eos-iws-*n convention: {veth_ns}"
    );
    Ok(())
}

#[test]
fn isolated_to_isolated_same_port_matrix() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);
    let port = unique_port();
    let caller_a = format!("iws-net-a-{}", eos_e2e_test::unique_suffix());
    let caller_b = format!("iws-net-b-{}", eos_e2e_test::unique_suffix());

    lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller_a}),
    )?;
    lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller_b}),
    )?;

    let mut sessions: Vec<(String, String)> = Vec::new();
    let body = (|| -> Result<()> {
        let server_a = start_bound_socket(&lease, &caller_a, port, "A")?;
        ensure!(
            as_str(&server_a, "status")? == "running",
            "caller A isolated server must stay running: {server_a}"
        );
        wait_for_output(&lease, Some(&caller_a), &server_a, "BOUND-A")
            .with_context(|| format!("caller A must bind the selected port: {server_a}"))?;
        sessions.push((
            caller_a.clone(),
            as_str(&server_a, "command_session_id")?.to_owned(),
        ));

        let server_b = start_bound_socket(&lease, &caller_b, port, "B")?;
        ensure!(
            as_str(&server_b, "status")? == "running",
            "caller B isolated server must also stay running on the same port: {server_b}"
        );
        wait_for_output(&lease, Some(&caller_b), &server_b, "BOUND-B")
            .with_context(|| format!("caller B must bind the same port: {server_b}"))?;
        sessions.push((
            caller_b.clone(),
            as_str(&server_b, "command_session_id")?.to_owned(),
        ));

        let same_namespace = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "caller_id": caller_a,
                "cmd": bind_probe_command(port, "SAME_CALLER_BOUND"),
                "yield_time_ms": 2000,
                "timeout_seconds": 30,}),
        )?;
        wait_for_output(&lease, Some(&caller_a), &same_namespace, "EADDRINUSE").with_context(
            || format!("a second bind inside caller A's isolated netns must conflict: {same_namespace}"),
        )?;
        ensure!(
            !stdout(&same_namespace).contains("SAME_CALLER_BOUND"),
            "same-caller bind must not succeed while the first caller A server is live: {same_namespace}"
        );
        Ok(())
    })();

    for (caller_id, session_id) in &sessions {
        cancel(&lease, Some(caller_id), session_id);
    }
    let _ = lease.call(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_b, "grace_s": 0.1}),
    );
    let _ = lease.call(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_a, "grace_s": 0.1}),
    );
    body
}
