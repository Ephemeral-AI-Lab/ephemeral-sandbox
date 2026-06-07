use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Result};
use eos_e2e_test::NodeLease;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_bool, as_str, live_pool_or_skip, stdout, wait_for_active_leases,
    wait_for_session_count,
};

#[test]
fn iws_same_port_discard() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let server_cmd =
        "mkdir -p /eos/scratch/e2e && python3 -m http.server 39001 >/eos/scratch/e2e/eos-e2e-http.log 2>&1";
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let first = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": server_cmd,
            "yield_time_ms": 100,
            "timeout_seconds": 120,
            "max_output_tokens": 500
        }),
    )?;
    assert_eq!(
        as_str(&first, "status")?,
        "running",
        "isolated command should start: {first}"
    );
    let first_id = as_str(&first, "command_session_id")?.to_owned();
    lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": first_id}),
    )?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}))?;

    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let second = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": server_cmd,
            "yield_time_ms": 100,
            "timeout_seconds": 120,
            "max_output_tokens": 500
        }),
    )?;
    assert_eq!(
        as_str(&second, "status")?,
        "running",
        "same isolated port should be reusable after exit discard: {second}"
    );
    if let Some(id) = second
        .get("command_session_id")
        .and_then(serde_json::Value::as_str)
    {
        lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": id}),
        )?;
    }
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}))?;
    Ok(())
}

#[test]
fn iws_prompt_stdin_poll_cancel_private_discard() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = format!(
        "iws-command-session/prompt-{}.txt",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    let cmd = format!(
        "python3 -u -c 'import pathlib,sys,time; \
print(\"iws-prompt\", flush=True); \
payload=sys.stdin.readline().strip(); \
path=pathlib.Path({path:?}); \
path.parent.mkdir(parents=True, exist_ok=True); \
path.write_text(payload + \"\\n\"); \
print(\"iws-wrote:\" + payload, flush=True); \
time.sleep(60)'"
    );

    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": cmd,
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 1000
        }),
    )?;
    ensure!(
        as_str(&started, "status")? == "running" && stdout(&started).contains("iws-prompt"),
        "isolated prompt command should start and expose prompt: {started}"
    );
    let session_id = as_str(&started, "command_session_id")?.to_owned();

    let body = (|| -> Result<()> {
        let answered = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "private-payload\n",
                "yield_time_ms": 1500,
                "max_output_tokens": 1000
            }),
        )?;
        ensure!(
            !stdout(&answered).contains("iws-prompt"),
            "stdin cursor must not replay the already-consumed prompt: {answered}"
        );
        let reply = if stdout(&answered).contains("iws-wrote:private-payload") {
            answered
        } else {
            poll_stdin_cursor_until_stdout_contains(
                &lease,
                &session_id,
                "iws-wrote:private-payload",
                "iws-prompt",
                Instant::now() + Duration::from_secs(15),
            )?
        };
        ensure!(
            stdout(&reply).contains("iws-wrote:private-payload"),
            "stdin write should drive the isolated prompt command: {reply}"
        );

        let read_private = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": &path}))?;
        ensure!(
            as_str(&read_private, "workspace")? == "isolated",
            "read while isolated should route through isolated workspace: {read_private}"
        );
        ensure!(
            as_str(&read_private, "content")? == "private-payload\n",
            "isolated command-session write should be visible while open: {read_private}"
        );

        let quiet = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "",
                "yield_time_ms": 250,
                "max_output_tokens": 1000
            }),
        )?;
        ensure!(
            !stdout(&quiet).contains("iws-wrote:private-payload"),
            "empty poll must not replay consumed isolated command output: {quiet}"
        );

        let not_done = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [session_id.clone()]}),
        )?;
        ensure!(
            array(&not_done, "completions")?.is_empty(),
            "sleeping isolated command should not collect before cancellation: {not_done}"
        );

        let cancelled = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 1000}),
        )?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "isolated command cancel should return terminal-ish status: {cancelled}"
        );
        wait_for_session_count(&lease, 0)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 1000}),
        );
    }
    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.1}));
    body?;
    exit?;

    let after_exit = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": &path}))?;
    ensure!(
        as_str(&after_exit, "workspace")? == "ephemeral",
        "read after isolated exit should route back to ephemeral workspace: {after_exit}"
    );
    ensure!(
        !as_bool(&after_exit, "exists")?,
        "isolated command-session write should be discarded after exit: {after_exit}"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

fn poll_stdin_cursor_until_stdout_contains(
    lease: &NodeLease<'_>,
    session_id: &str,
    needle: &str,
    forbidden_replay: &str,
    deadline: Instant,
) -> Result<Value> {
    let mut last = None;
    while Instant::now() < deadline {
        let poll = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "",
                "yield_time_ms": 250,
                "max_output_tokens": 1000
            }),
        )?;
        ensure!(
            !stdout(&poll).contains(forbidden_replay),
            "stdin cursor poll must not replay isolated prompt output: {poll}"
        );
        if stdout(&poll).contains(needle) {
            return Ok(poll);
        }
        last = Some(poll);
    }
    bail!("stdin cursor did not surface {needle:?} before deadline; last poll: {last:?}");
}
