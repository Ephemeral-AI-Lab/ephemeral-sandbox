use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Result};
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::support::{
    array, as_i64, as_str, clean_stdout, finalize_foreground_command, live_pool_or_skip, stdout,
    wait_for_active_leases, wait_for_command_session_transcript_recycled, wait_for_session_count,
};

#[test]
fn nonzero_exit_and_stderr_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let failed = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'printf stdout-before; printf stderr-before >&2; exit 42'",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
    )?;
    // Under emulation a slow ns-runner spawn can outlast the yield, so the
    // command returns "running"; finalize it to its terminal outcome first.
    let failed =
        finalize_foreground_command(&lease, failed, Instant::now() + Duration::from_secs(20))?;
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
fn stderr_and_stdin_output_keep_long_lived_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"stderr-ready\", file=sys.stderr, flush=True); payload=sys.stdin.readline().strip(); print(\"stderr-reply:\" + payload, file=sys.stderr, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "stderr prompt command should keep running after first stderr output: {started}"
    );
    ensure!(
        stdout(&started).contains("stderr-ready"),
        "PTY stdout stream should expose initial stderr output: {started}"
    );
    ensure!(
        stderr(&started).is_empty(),
        "stderr field should stay empty for merged PTY output: {started}"
    );
    let session_id = as_str(&started, "command_id")?.to_owned();

    let body = (|| -> Result<()> {
        let answered = lease.call_ok(
            catalog::SANDBOX_COMMAND_WRITE_STDIN,
            json!({
                "command_id": &session_id,
                "chars": "payload\n",
                "yield_time_ms": 1500,}),
        )?;
        ensure!(
            as_str(&answered, "status")? == "running",
            "stdin reply on a long-lived stderr command should remain running: {answered}"
        );
        ensure!(
            !stdout(&answered).contains("stderr-ready"),
            "stdin output should be scoped to text produced after the write: {answered}"
        );
        let reply = if stdout(&answered).contains("stderr-reply:payload") {
            answered
        } else {
            poll_read_progress_until_stdout_contains(
                &lease,
                &session_id,
                "stderr-reply:payload",
                Instant::now() + Duration::from_secs(10),
            )?
        };
        ensure!(
            stdout(&reply).contains("stderr-reply:payload"),
            "PTY stdout stream should expose stderr produced after stdin: {reply}"
        );

        let not_done = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({"command_ids": [session_id.clone()]}),
        )?;
        ensure!(
            array(&not_done, "completions")?.is_empty(),
            "sleeping stderr/stdin command must not collect before cancellation: {not_done}"
        );

        let cancelled = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &session_id}),
        )?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "cancel should return terminal-ish status after long-lived stderr/stdin output: {cancelled}"
        );
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_session_transcript_recycled(&lease, &session_id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &session_id}),
        );
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

#[test]
fn missing_command_and_invalid_session_ids_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let missing = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "definitely_missing_eos_e2e_command",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
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
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": bogus,
            "chars": "ignored\n",
            "yield_time_ms": 100,}),
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
        catalog::SANDBOX_COMMAND_CANCEL,
        json!({"command_id": bogus}),
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
        catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
        json!({"command_ids": [bogus]}),
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
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "python3 -u - <<'PY'\nimport sys, time\nsys.stdout.write('Ω' * 20000)\nsys.stdout.flush()\ntime.sleep(60)\nPY",
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "large-output command should stay running for transcript/backpressure checks: {started}"
    );
    ensure!(
        stdout(&started).contains('Ω'),
        "initial output should expose the timestamped transcript burst: {started}"
    );
    ensure_valid_utf8_prefix(&started)?;
    let session_id = as_str(&started, "command_id")?.to_owned();

    let body = (|| -> Result<()> {
        for _ in 0..2 {
            let poll = lease.call_ok(
                catalog::SANDBOX_COMMAND_POLL,
                json!({
                    "command_id": &session_id,
                    "last_n_lines": 1,
                }),
            )?;
            ensure!(
                stdout(&poll).contains('Ω'),
                "read_progress should return the timestamped transcript tail under backpressure: {poll}"
            );
            ensure_valid_utf8_prefix(&poll)?;
        }
        let cancelled = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &session_id}),
        )?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "cancel should return a terminal-ish status after output pressure: {cancelled}"
        );
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_session_transcript_recycled(&lease, &session_id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &session_id}),
        );
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

fn ensure_valid_utf8_prefix(response: &Value) -> Result<()> {
    // Strip the per-line `[ISO-8601] ` transcript timestamp prefix; the property
    // under test is that the Ω burst keeps its codepoint boundaries (a split Ω
    // would surface as U+FFFD `�`, which still fails the char check below).
    let output = clean_stdout(response);
    ensure!(
        output
            .chars()
            .all(|ch| ch == 'Ω' || ch == '\r' || ch == '\n'),
        "capped output should preserve UTF-8 codepoint boundaries: {response}"
    );
    Ok(())
}

fn poll_read_progress_until_stdout_contains(
    lease: &eos_e2e_test::NodeLease<'_>,
    session_id: &str,
    needle: &str,
    deadline: Instant,
) -> Result<Value> {
    let mut last = None;
    while Instant::now() < deadline {
        let poll = lease.call_ok(
            catalog::SANDBOX_COMMAND_POLL,
            json!({
                "command_id": session_id,
                "last_n_lines": 8,
            }),
        )?;
        if stdout(&poll).contains(needle) {
            return Ok(poll);
        }
        last = Some(poll);
    }
    bail!("read_progress did not surface {needle:?} before deadline; last poll: {last:?}");
}

fn stderr(value: &Value) -> &str {
    value
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(Value::as_str)
        .or_else(|| value.get("stderr").and_then(Value::as_str))
        .unwrap_or_default()
}

#[test]
fn stdin_to_non_reading_consumer_stays_bounded_and_cancellable() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A consumer that never reads stdin. A small stdin write fits the PTY buffer
    // and returns immediately while the session stays cancellable. The over-buffer
    // case (where the non-blocking writer must bound the push by a deadline) is
    // covered by `over_buffer_stdin_to_non_reading_consumer_returns_backpressure`.
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo no-read-ready; sleep 60'",
            "yield_time_ms": 800,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "non-reading consumer should start: {started}"
    );
    let id = as_str(&started, "command_id")?.to_owned();

    let payload = format!("{}\n", "x".repeat(1024));
    let write_started = Instant::now();
    let wrote = lease.call_ok(
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": &id,
            "chars": payload,
            "yield_time_ms": 300,}),
    )?;
    ensure!(
        as_str(&wrote, "status")? == "running",
        "session should stay running after stdin to a non-reading consumer: {wrote}"
    );
    ensure!(
        write_started.elapsed() < Duration::from_secs(10),
        "a bounded stdin write must return promptly, not wedge: took {:?}",
        write_started.elapsed()
    );

    let cancelled = lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": &id}))?;
    ensure!(
        matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
        "session must stay cancellable after stdin pressure: {cancelled}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn over_buffer_stdin_to_non_reading_consumer_returns_backpressure() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A consumer that never reads stdin, plus a payload far larger than the kernel
    // PTY input buffer. The non-blocking writer must bound the push by a deadline
    // and return a structured backpressure error instead of wedging, and the
    // session must stay cancellable. (Before the non-blocking rewrite this write
    // blocked until the session timeout.)
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo no-read-ready; sleep 60'",
            "yield_time_ms": 800,
            "timeout_seconds": 120,
        }),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "non-reading consumer should start: {started}"
    );
    let id = as_str(&started, "command_id")?.to_owned();

    // Many newline-terminated lines, far past the ~4 KiB cooked PTY input buffer.
    // A single overlong line would be dropped past MAX_CANON without blocking; only
    // accumulated unread lines fill the input queue and exert real backpressure.
    let payload = "eos-e2e-backpressure-line\n".repeat(16384);
    let write_started = Instant::now();
    let pushed = lease.call(
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": &id,
            "chars": payload,
            "yield_time_ms": 300,
        }),
    )?;
    let elapsed = write_started.elapsed();
    ensure!(
        elapsed < Duration::from_secs(15),
        "over-buffer stdin must return bounded, not wedge: took {elapsed:?}"
    );
    ensure!(
        pushed.to_string().contains("backpressure"),
        "over-buffer stdin to a non-reading consumer should surface a backpressure diagnostic: {pushed}"
    );

    let cancelled = lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": &id}))?;
    ensure!(
        matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
        "session must stay cancellable after backpressure: {cancelled}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}
