use anyhow::{ensure, Result};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_i64, as_str, live_pool_or_skip, stdout, wait_for_active_leases,
    wait_for_session_count,
};

#[test]
fn nonzero_exit_and_stderr_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let failed = lease.call(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'printf stdout-before; printf stderr-before >&2; exit 42'",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 2000
        }),
    )?;
    ensure!(
        as_str(&failed, "status")? == "error",
        "nonzero command should return an error status: {failed}"
    );
    ensure!(
        as_i64(&failed, "exit_code")? == 42,
        "nonzero command should preserve its exit code: {failed}"
    );
    let output = stdout(&failed);
    ensure!(
        output.contains("stdout-before") && output.contains("stderr-before"),
        "PTY output should merge stdout and stderr into the model stream: {failed}"
    );
    ensure!(
        stderr(&failed).is_empty(),
        "stderr field should stay empty for merged PTY output: {failed}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn missing_command_and_invalid_session_ids_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let missing = lease.call(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "definitely_missing_eos_e2e_command",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 2000
        }),
    )?;
    ensure!(
        as_str(&missing, "status")? == "error",
        "missing command should return an error status: {missing}"
    );
    ensure!(
        as_i64(&missing, "exit_code")? != 0,
        "missing command should preserve a nonzero exit code: {missing}"
    );
    ensure!(
        stdout(&missing).contains("not found") || stderr(&missing).contains("not found"),
        "missing command should expose shell diagnostic output: {missing}"
    );

    let bogus = format!(
        "missing-session-{}",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    let stdin = lease.call(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": bogus,
            "chars": "ignored\n",
            "yield_time_ms": 100,
            "max_output_tokens": 200
        }),
    )?;
    ensure!(
        as_str(&stdin, "status")? == "error",
        "write_stdin against an unknown session should return a structured error: {stdin}"
    );
    ensure!(
        stderr(&stdin).contains("command_session_not_found"),
        "write_stdin unknown-session error should carry a stable diagnostic: {stdin}"
    );

    let cancel = lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": bogus, "max_output_tokens": 200}),
    )?;
    ensure!(
        as_str(&cancel, "status")? == "error",
        "cancel against an unknown session should return a structured error: {cancel}"
    );
    ensure!(
        stderr(&cancel).contains("command_session_not_found"),
        "cancel unknown-session error should carry a stable diagnostic: {cancel}"
    );

    let collect = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({"command_session_ids": [bogus]}),
    )?;
    ensure!(
        array(&collect, "completions")?.is_empty(),
        "collect_completed for an unknown session should be an empty read, not an error: {collect}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn output_backpressure_preserves_utf8_and_drains_on_cancel() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 -u - <<'PY'\nimport sys, time\nsys.stdout.write('Ω' * 20000)\nsys.stdout.flush()\ntime.sleep(60)\nPY",
            "yield_time_ms": 500,
            "timeout_seconds": 120,
            "max_output_tokens": 24
        }),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "large-output command should stay running for cursor/backpressure checks: {started}"
    );
    ensure!(
        stdout(&started).len() < 20_000,
        "initial output should be capped instead of returning the full burst: {started}"
    );
    ensure_valid_utf8_prefix(&started)?;
    let session_id = as_str(&started, "command_session_id")?.to_owned();

    let body = (|| -> Result<()> {
        for _ in 0..2 {
            let poll = lease.call_ok(
                ops::API_V1_WRITE_STDIN,
                json!({
                    "command_session_id": session_id,
                    "chars": "",
                    "yield_time_ms": 150,
                    "max_output_tokens": 24
                }),
            )?;
            ensure!(
                stdout(&poll).len() < 20_000,
                "cursor poll should remain output-capped under backpressure: {poll}"
            );
            ensure_valid_utf8_prefix(&poll)?;
        }
        let cancelled = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 200}),
        )?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "cancel should return a terminal-ish status after output pressure: {cancelled}"
        );
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 200}),
        );
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

fn ensure_valid_utf8_prefix(response: &Value) -> Result<()> {
    let output = stdout(response);
    ensure!(
        output
            .chars()
            .all(|ch| ch == 'Ω' || ch == '\r' || ch == '\n'),
        "capped output should preserve UTF-8 codepoint boundaries: {response}"
    );
    Ok(())
}

fn stderr(value: &Value) -> &str {
    value
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(Value::as_str)
        .or_else(|| value.get("stderr").and_then(Value::as_str))
        .unwrap_or_default()
}
