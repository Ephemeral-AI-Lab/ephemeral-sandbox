#[path = "../common/mod.rs"]
mod common;

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_protocol::ops;
use serde_json::{json, Value};

use common::{array, as_i64, as_str, live_pool_or_skip, stdout};

fn start_sleeping_session(lease: &eos_e2e_test::NodeLease<'_>, marker: &str) -> Result<String> {
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("sh -c 'echo {marker}; sleep 60'"),
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 1000
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running");
    assert!(
        stdout(&started).contains(marker),
        "session should print marker before returning: {started}"
    );
    Ok(as_str(&started, "command_session_id")?.to_owned())
}

fn cancel_session(lease: &eos_e2e_test::NodeLease<'_>, id: &str) -> Result<Value> {
    lease.call_ok(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": id, "max_output_tokens": 1000}),
    )
}

#[test]
fn exec_simple() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({"cmd": "true", "yield_time_ms": 1000, "timeout_seconds": 5}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);
    assert_eq!(stdout(&exec), "");
    Ok(())
}

#[test]
fn exec_returns_session_id() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let id = start_sleeping_session(&lease, "session-started")?;
    assert!(!id.is_empty());
    cancel_session(&lease, &id)?;
    Ok(())
}

#[test]
fn write_stdin_echo() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"ready\", flush=True); line=sys.stdin.readline().strip(); print(\"got:\" + line, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 1000
        }),
    )?;
    let id = as_str(&started, "command_session_id")?.to_owned();
    let stdin = lease.call_ok(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": id,
            "chars": "payload\n",
            "yield_time_ms": 2000,
            "max_output_tokens": 1000
        }),
    )?;
    assert!(
        stdout(&stdin).contains("got:payload"),
        "stdin write should return command output: {stdin}"
    );
    cancel_session(&lease, &id)?;
    Ok(())
}

#[test]
fn collect_completed_drains() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo queued; sleep 1; echo done'",
            "yield_time_ms": 100,
            "timeout_seconds": 10,
            "max_output_tokens": 1000
        }),
    )?;
    let id = as_str(&started, "command_session_id")?.to_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let collected = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [id]}),
        )?;
        let completions = array(&collected, "completions")?;
        if let Some(completion) = completions.first() {
            assert_eq!(completion["command_session_id"], id);
            assert!(
                stdout(completion.get("result").context("completion result")?).contains("done"),
                "completion should carry final stdout: {completion}"
            );
            break;
        }
        if Instant::now() >= deadline {
            bail!("session completion was not parked before deadline");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let redelivered = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({"command_session_ids": [id]}),
    )?;
    assert!(
        array(&redelivered, "completions")?.is_empty(),
        "collect_completed should remove delivered completions: {redelivered}"
    );
    Ok(())
}

#[test]
fn cancel_unblocks() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let id = start_sleeping_session(&lease, "cancel-ready")?;
    let cancel = cancel_session(&lease, &id)?;
    assert!(
        matches!(as_str(&cancel, "status")?, "cancelled" | "error" | "ok"),
        "cancel should return a terminal-ish status: {cancel}"
    );
    Ok(())
}

#[test]
fn session_count_accuracy() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let first = start_sleeping_session(&lease, "count-one")?;
    let second = start_sleeping_session(&lease, "count-two")?;
    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(
        as_i64(&count, "count")?,
        2,
        "two live sessions expected: {count}"
    );
    cancel_session(&lease, &first)?;
    cancel_session(&lease, &second)?;
    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(
        as_i64(&count, "count")?,
        0,
        "cancel should remove sessions: {count}"
    );
    Ok(())
}

#[test]
fn exec_timeout() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({"cmd": "sleep 2", "yield_time_ms": 2500, "timeout_seconds": 1}),
    )?;
    assert!(
        matches!(as_str(&exec, "status")?, "timeout" | "error" | "cancelled"),
        "timeout path should return a non-ok status: {exec}"
    );
    Ok(())
}

#[test]
fn output_token_cap() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 - <<'PY'\nimport sys, time\nsys.stdout.write('x' * 20000)\nsys.stdout.flush()\ntime.sleep(60)\nPY",
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 20
        }),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running");
    assert!(
        stdout(&exec).len() < 20_000,
        "max_output_tokens should cap returned stdout: {} bytes",
        stdout(&exec).len()
    );
    let id = as_str(&exec, "command_session_id")?;
    lease.call_ok(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": id}),
    )?;
    Ok(())
}

#[test]
fn cancel_by_invocation_id_reports_already_done_for_idle_id() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let cancel = lease.call_ok(
        ops::API_V1_CANCEL,
        json!({"invocation_id": "eos-e2e-not-running"}),
    )?;
    assert_eq!(cancel["already_done"], Value::Bool(true));
    assert_eq!(cancel["cancelled"], Value::Bool(false));
    Ok(())
}
