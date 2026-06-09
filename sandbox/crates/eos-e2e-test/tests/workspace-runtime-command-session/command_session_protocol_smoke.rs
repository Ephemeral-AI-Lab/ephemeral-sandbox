use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::unique_suffix;
use eos_protocol::ops;
use serde_json::json;

use crate::support::{
    as_i64, as_str, live_pool_or_skip, wait_for_active_leases,
    wait_for_command_session_transcript_recycled, wait_for_command_stdout_contains,
};

fn command_line_marker_count(lease: &eos_e2e_test::NodeLease<'_>, marker: &str) -> Result<i64> {
    let script = format!(
        r#"import os
import pathlib

marker = {marker:?}
count = 0
for proc in pathlib.Path("/proc").iterdir():
    if not proc.name.isdigit() or int(proc.name) == os.getpid():
        continue
    try:
        cmdline = proc.joinpath("cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "ignore")
    except OSError:
        continue
    if marker in cmdline:
        count += 1
print(count)
"#
    );
    let output = lease.container().exec(&["python3", "-c", &script])?;
    output
        .trim()
        .parse::<i64>()
        .with_context(|| format!("parse marker count from {output:?}"))
}

fn wait_for_command_line_marker_count(
    lease: &eos_e2e_test::NodeLease<'_>,
    marker: &str,
    expected: i64,
) -> Result<i64> {
    // Widened for x86-on-arm64 emulation: a backgrounded descendant can lag into
    // /proc, and post-cancel process-group reaping can lag out of it.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let count = command_line_marker_count(lease, marker)?;
        if count == expected {
            return Ok(count);
        }
        if Instant::now() >= deadline {
            bail!("marker {marker} count did not reach {expected}; last count {count}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn command_sessions_accept_stdin_and_release_on_cancel() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(as_i64(&count, "count")?, 0);

    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"ready\", flush=True); line=sys.stdin.readline().strip(); print(\"got:\" + line, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(as_str(&started, "status")?, "running");
    let session_id = as_str(&started, "command_session_id")?.to_owned();
    assert!(
        started["output"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("ready"),
        "expected initial stdout to contain readiness marker: {started}"
    );
    let leased = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(
        as_i64(&leased, "active_leases")? >= 1,
        "running command should hold a layer lease: {leased}"
    );

    let stdin = lease.call_ok(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": session_id,
            "chars": "line-one\n",
            "yield_time_ms": 2000,}),
    )?;
    if as_str(&stdin, "status")? != "running" {
        bail!("expected session to keep running after stdin write: {stdin}");
    }
    assert!(
        stdin["output"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("got:line-one"),
        "expected stdin echo in stdout: {stdin}"
    );

    let cancel = lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": &session_id}),
    )?;
    assert!(matches!(
        as_str(&cancel, "status")?,
        "cancelled" | "ok" | "error"
    ));

    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(as_i64(&count, "count")?, 0);
    let released = wait_for_active_leases(&lease, 0)?;
    assert_eq!(
        as_i64(&released, "active_leases")?,
        0,
        "cancelled command should release its layer lease: {released}"
    );
    wait_for_command_session_transcript_recycled(&lease, &session_id)?;
    Ok(())
}

#[test]
fn command_sessions_cancel_cleans_descendant_processes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = format!("eos_e2e_descendant_{}", unique_suffix().replace('-', "_"));
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("bash -lc 'bash -c \"exec -a {marker} sleep 60\" & echo descendant-ready; wait'"),
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(as_str(&started, "status")?, "running");
    let session_id = as_str(&started, "command_session_id")?.to_owned();
    // Emulation can slip the `echo descendant-ready` past the first 500ms yield;
    // poll the transcript for it instead of reading only the initial snapshot.
    wait_for_command_stdout_contains(&lease, &session_id, "descendant-ready")?;
    // The session's own `bash -lc '...MARKER...'` carries the marker in its argv
    // alongside the `exec -a MARKER sleep 60` descendant, so poll for >= 1 marker
    // process (not an exact count) — it can also lag into /proc under emulation.
    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while command_line_marker_count(&lease, &marker)? < 1 {
        if Instant::now() >= marker_deadline {
            bail!("expected at least one live descendant marker process before cancel");
        }
        thread::sleep(Duration::from_millis(50));
    }

    let cancel = lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": &session_id}),
    )?;
    assert!(matches!(
        as_str(&cancel, "status")?,
        "cancelled" | "ok" | "error"
    ));
    wait_for_command_line_marker_count(&lease, &marker, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &session_id)?;
    Ok(())
}
