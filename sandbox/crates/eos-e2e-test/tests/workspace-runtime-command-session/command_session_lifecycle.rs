use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::{unique_suffix, NodeLease};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_i64, as_str, command_session_transcript_logs, command_session_transcript_path,
    live_pool_or_skip, settle_foreground_command, stdout, wait_for_active_leases,
    wait_for_command_session_transcript_recycled, wait_for_container_path, wait_for_session_count,
};

fn start_sleeping_session(lease: &eos_e2e_test::NodeLease<'_>, marker: &str) -> Result<String> {
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("sh -c 'echo {marker}; sleep 60'"),
            "yield_time_ms": 500,
            "timeout_seconds": 120
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
    let cancelled = lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": id}),
    )?;
    wait_for_command_session_transcript_recycled(lease, id)?;
    Ok(cancelled)
}

fn wait_for_transcript_logs(lease: &NodeLease<'_>, expected: &[String]) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let current = command_session_transcript_logs(lease)?;
        if current == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("transcript logs did not settle at {expected:?}; last {current:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn process_marker() -> String {
    format!(
        "eos_e2e_command_session_{}",
        unique_suffix().replace('-', "_")
    )
}

fn assert_teardown_control_reaps_marker_process(
    lease: &NodeLease<'_>,
    label: &str,
    chars: &str,
) -> Result<()> {
    let marker = process_marker();
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!(
                "bash -lc 'bash -c \"exec -a {marker} sleep 60\" & python3 -u -c \"import sys,time; print(\\\"{label}-ready\\\", flush=True); sys.stdin.readline(); time.sleep(60)\"'"
            ),
            "yield_time_ms": 1500,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    assert!(
        stdout(&started).contains(&format!("{label}-ready")),
        "stdin reader should be ready before {label} teardown: {started}"
    );
    let id = as_str(&started, "command_session_id")?.to_owned();
    wait_for_marker_at_least(lease, &marker, 1)?;

    let cancelled = lease.call(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": &id,
            "chars": chars,
            "yield_time_ms": 3000
        }),
    )?;
    assert_eq!(
        as_str(&cancelled, "status")?,
        "cancelled",
        "{label} should route to command-session cancel: {cancelled}"
    );
    // When the cancel reaps the child within cancel_wait_ms the daemon returns
    // the reaped shape (exit_code 130). Under qemu emulation the reap can exceed
    // that window, so the daemon returns the inline cancelled response with a
    // null exit_code instead. Both are valid cancelled shapes; the authoritative
    // teardown (process group killed, session + leases drained) is verified by
    // the marker/session/lease waits below, so accept either exit shape here.
    if let Some(exit_code) = cancelled.get("exit_code").and_then(Value::as_i64) {
        assert_eq!(
            exit_code, 130,
            "{label} cancelled exit_code, when present, must be 130: {cancelled}"
        );
    }
    wait_for_session_count(lease, 0)?;
    wait_for_command_session_transcript_recycled(lease, &id)?;
    wait_for_active_leases(lease, 0)?;
    wait_for_marker_count(lease, &marker, 0, Duration::from_secs(3))?;
    Ok(())
}

fn assert_timestamped_lines(output: &str, context: &Value) {
    let normalized = output.replace('\r', "");
    let lines = normalized
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert!(!lines.is_empty(), "expected timestamped output: {context}");
    for line in lines {
        assert!(
            has_timestamp_prefix(line),
            "output line should start with timestamp prefix, got {line:?}: {context}"
        );
    }
}

fn has_timestamp_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 27 {
        return false;
    }
    let fixed = bytes[0] == b'['
        && bytes[5] == b'-'
        && bytes[8] == b'-'
        && bytes[11] == b'T'
        && bytes[14] == b':'
        && bytes[17] == b':'
        && bytes[20] == b'.';
    fixed
        && ((bytes[24] == b'Z' && bytes[25] == b']' && bytes[26] == b' ')
            || (bytes.len() >= 32
                && matches!(bytes[24], b'+' | b'-')
                && bytes[27] == b':'
                && bytes[30] == b']'
                && bytes[31] == b' '))
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
fn exec_command_outputs_timestamped_transcript_lines() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let completed = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "printf 'stamp-one\\nstamp-two\\n'",
            "yield_time_ms": 2000,
            "timeout_seconds": 30
        }),
    )?;
    let completed =
        settle_foreground_command(&lease, completed, Instant::now() + Duration::from_secs(30))?;
    assert_eq!(as_str(&completed, "status")?, "ok", "{completed}");
    assert!(
        stdout(&completed).contains("stamp-one") && stdout(&completed).contains("stamp-two"),
        "completed command should return its transcript output: {completed}"
    );
    assert_timestamped_lines(stdout(&completed), &completed);
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
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
            "cmd": "python3 -u -c 'import sys; print(\"ready\", flush=True); line=sys.stdin.readline().strip(); print(\"got:\" + line, flush=True)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120
        }),
    )?;
    let id = as_str(&started, "command_session_id")?.to_owned();
    let transcript_path = command_session_transcript_path(&id);
    wait_for_container_path(&lease, &transcript_path, true, Duration::from_secs(3))?;
    let stdin = lease.call_ok(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": &id,
            "chars": "payload\n",
            "yield_time_ms": 2000
        }),
    )?;
    assert_eq!(
        as_str(&stdin, "status")?,
        "ok",
        "stdin write should let the command exit naturally: {stdin}"
    );
    assert!(
        stdout(&stdin).contains("ready") && stdout(&stdin).contains("got:payload"),
        "stdin-triggered completion should return the full captured output: {stdin}"
    );
    assert_timestamped_lines(stdout(&stdin), &stdin);
    wait_for_session_count(&lease, 0)?;
    wait_for_container_path(&lease, &transcript_path, false, Duration::from_secs(3))?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn read_command_progress_returns_stateless_tail_snapshot() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"progress-first\", flush=True); line=sys.stdin.readline().strip(); print(\"progress-second:\" + line, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running");
    assert!(
        stdout(&started).contains("progress-first"),
        "initial poll should return first output: {started}"
    );
    let id = as_str(&started, "command_session_id")?.to_owned();

    let second = lease.call_ok(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": &id,
            "chars": "payload\n",
            "yield_time_ms": 1500
        }),
    )?;
    assert!(
        stdout(&second).contains("progress-second:payload"),
        "stdin write should return newly produced output: {second}"
    );
    assert!(
        !stdout(&second).contains("progress-first"),
        "stdin write must not replay already consumed output: {second}"
    );

    let progress = lease.call_ok(
        ops::API_V1_COMMAND_READ_PROGRESS,
        json!({
            "command_session_id": &id,
            "last_n_lines": 10
        }),
    )?;
    assert!(
        stdout(&progress).contains("progress-first")
            && stdout(&progress).contains("progress-second:payload"),
        "progress reads are stateless tail snapshots: {progress}"
    );
    let tail = lease.call_ok(
        ops::API_V1_COMMAND_READ_PROGRESS,
        json!({
            "command_session_id": &id,
            "last_n_lines": 1
        }),
    )?;
    assert!(
        stdout(&tail).contains("progress-second:payload")
            && !stdout(&tail).contains("progress-first"),
        "last_n_lines should bound the read-progress tail without consuming state: {tail}"
    );
    cancel_session(&lease, &id)?;
    Ok(())
}

#[test]
fn read_command_progress_finalizes_completed_session_and_recycles_transcript() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo progress-start; sleep 1; echo progress-end'",
            "yield_time_ms": 100,
            "timeout_seconds": 10
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    let id = as_str(&started, "command_session_id")?.to_owned();
    let transcript_path = command_session_transcript_path(&id);
    wait_for_container_path(&lease, &transcript_path, true, Duration::from_secs(3))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = lease.call_ok(
            ops::API_V1_COMMAND_READ_PROGRESS,
            json!({
                "command_session_id": &id,
                "last_n_lines": 10
            }),
        )?;
        if as_str(&progress, "status")? == "ok" {
            assert!(
                stdout(&progress).contains("progress-start")
                    && stdout(&progress).contains("progress-end"),
                "read_progress completion should return the final output tail: {progress}"
            );
            break;
        }
        if Instant::now() >= deadline {
            bail!("read_progress did not finalize the completed session: {progress}");
        }
        thread::sleep(Duration::from_millis(100));
    }
    wait_for_session_count(&lease, 0)?;
    wait_for_container_path(&lease, &transcript_path, false, Duration::from_secs(3))?;
    wait_for_active_leases(&lease, 0)?;
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
            "timeout_seconds": 10
        }),
    )?;
    let id = as_str(&started, "command_session_id")?.to_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let collected = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [&id]}),
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
        json!({"command_session_ids": [&id]}),
    )?;
    assert!(
        array(&redelivered, "completions")?.is_empty(),
        "collect_completed should remove delivered completions: {redelivered}"
    );
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    Ok(())
}

#[test]
fn collect_completed_preserves_full_timestamped_transcript() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let first = format!(
        "transcript-full-first-{}",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    let last = format!(
        "transcript-full-last-{}",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    let cmd = format!(
        "sh -c 'echo {first}; sleep 1; yes filler | head -c 1200000; printf \"\\n{last}\\n\"'"
    );
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": cmd,
            "yield_time_ms": 100,
            "timeout_seconds": 10
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    assert!(
        stdout(&started).contains(&first),
        "initial yield should consume the first marker: {started}"
    );
    let id = as_str(&started, "command_session_id")?.to_owned();

    let completion = collect_completion(&lease, &id, Duration::from_secs(10))?;
    let result = completion.get("result").context("completion result")?;
    let output = stdout(result);
    assert!(
        output.contains(&first),
        "completion stdout lost transcript prefix; bytes={}",
        output.len()
    );
    assert!(
        output.contains(&last),
        "completion stdout lost transcript suffix; bytes={}",
        output.len()
    );
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    Ok(())
}

#[test]
fn finite_exec_before_yield_recycles_transient_transcript_file() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let before = command_session_transcript_logs(&lease)?;
    let marker = format!(
        "finite-transcript-{}",
        eos_e2e_test::unique_suffix().replace('-', "_")
    );
    let completed = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("printf '{marker}-a\\n{marker}-b\\n{marker}-c\\n'"),
            "yield_time_ms": 3000
        }),
    )?;
    assert_eq!(
        as_str(&completed, "status")?,
        "ok",
        "finite command should complete inside the initial yield: {completed}"
    );
    assert!(
        completed.get("command_session_id").is_none(),
        "finite command should not expose a background session handle: {completed}"
    );
    assert!(
        stdout(&completed).contains(&format!("{marker}-a"))
            && stdout(&completed).contains(&format!("{marker}-b"))
            && stdout(&completed).contains(&format!("{marker}-c")),
        "finite command should return its full stdout in the initial response: {completed}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    wait_for_transcript_logs(&lease, &before)?;
    Ok(())
}

#[test]
fn completed_session_removes_transcript_file() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo transcript-start; sleep 1; echo transcript-end'",
            "yield_time_ms": 100,
            "timeout_seconds": 30
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    let id = as_str(&started, "command_session_id")?.to_owned();
    let transcript_path = command_session_transcript_path(&id);
    wait_for_container_path(&lease, &transcript_path, true, Duration::from_secs(3))?;

    let completion = collect_completion(&lease, &id, Duration::from_secs(10))?;
    let result = completion.get("result").context("completion result")?;
    assert!(
        stdout(result).contains("transcript-start") && stdout(result).contains("transcript-end"),
        "completion should carry the final stdout: {completion}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_container_path(&lease, &transcript_path, false, Duration::from_secs(3))?;
    wait_for_active_leases(&lease, 0)?;
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
    let before = command_session_transcript_logs(&lease)?;
    let start = Instant::now();
    let timed_out = lease.call(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sleep 5",
            "yield_time_ms": 2500
        }),
    )?;
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "omitted timeout should return before the 5s command can finish: {timed_out}"
    );
    assert!(
        matches!(
            as_str(&timed_out, "status")?,
            "timed_out" | "error" | "cancelled"
        ),
        "omitted timeout should use the suite default_timeout_s=1: {timed_out}"
    );
    assert!(
        matches!(as_i64(&timed_out, "exit_code")?, 124 | -9 | 130),
        "timeout may surface as runner timeout or daemon reaper kill: {timed_out}"
    );
    assert!(
        timed_out.get("command_session_id").is_none(),
        "foreground timeout should not expose a recycled background handle: {timed_out}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    wait_for_transcript_logs(&lease, &before)?;
    Ok(())
}

#[test]
fn output_burst_returns_full_timestamped_transcript() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "python3 - <<'PY'\nimport sys, time\nsys.stdout.write('x' * 20000)\nsys.stdout.flush()\ntime.sleep(60)\nPY",
            "yield_time_ms": 500,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running");
    assert!(
        stdout(&exec).len() >= 20_000,
        "exec_command should expose transcript-backed output; returned {} bytes",
        stdout(&exec).len()
    );
    assert_timestamped_lines(stdout(&exec), &exec);
    let id = as_str(&exec, "command_session_id")?;
    cancel_session(&lease, id)?;
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

#[test]
fn write_stdin_ctrl_d_reaps_marker_process() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    assert_teardown_control_reaps_marker_process(&lease, "ctrl-d", "\u{4}")
}

#[test]
fn nohup_child_keeps_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = process_marker();
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!(
                "bash -lc 'nohup bash -c \"exec -a {marker} sleep 60\" >/dev/null 2>&1 & echo nohup-ready'"
            ),
            "yield_time_ms": 1000,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(
        as_str(&started, "status")?,
        "running",
        "plain nohup stays in the runner process group and keeps the session live: {started}"
    );
    let id = as_str(&started, "command_session_id")?.to_owned();
    wait_for_session_stdout(&lease, &id, &started, "nohup-ready")?;
    wait_for_marker_at_least(&lease, &marker, 1)?;

    let collected = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({"command_session_ids": [id.clone()]}),
    )?;
    assert!(
        array(&collected, "completions")?.is_empty(),
        "nohup child in the same process group must not finalize early: {collected}"
    );

    cancel_session(&lease, &id)?;
    wait_for_marker_count(&lease, &marker, 0, Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn setsid_nohup_contract() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = process_marker();
    let completed = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!(
                "bash -lc 'setsid nohup bash -c \"exec -a {marker} sleep 4\" >/dev/null 2>&1 & echo setsid-ready'"
            ),
            "yield_time_ms": 2000,
            "timeout_seconds": 20
        }),
    )?;
    assert_eq!(
        as_str(&completed, "status")?,
        "ok",
        "setsid nohup escapes the runner process group, so the protocol command completes: {completed}"
    );
    assert!(
        completed.get("command_session_id").is_none(),
        "completed setsid command must not leave a command session handle: {completed}"
    );
    assert!(
        stdout(&completed).contains("setsid-ready"),
        "foreground shell should report the detached launch: {completed}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_marker_at_least(&lease, &marker, 1)?;
    wait_for_marker_count(&lease, &marker, 0, Duration::from_secs(6))?;
    Ok(())
}

fn wait_for_session_stdout(
    lease: &NodeLease<'_>,
    session_id: &str,
    initial: &Value,
    marker: &str,
) -> Result<()> {
    if stdout(initial).contains(marker) {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = initial.clone();
    loop {
        if Instant::now() >= deadline {
            bail!("session output never contained {marker}: {last}");
        }
        let poll = lease.call_ok(
            ops::API_V1_COMMAND_READ_PROGRESS,
            json!({
                "command_session_id": session_id,
                "last_n_lines": 20
            }),
        )?;
        if stdout(&poll).contains(marker) {
            return Ok(());
        }
        last = poll;
        thread::sleep(Duration::from_millis(50));
    }
}

fn marker_count(lease: &NodeLease<'_>, marker: &str) -> Result<i64> {
    let script = format!(
        r#"import os, pathlib
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

fn wait_for_marker_at_least(lease: &NodeLease<'_>, marker: &str, minimum: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let count = marker_count(lease, marker)?;
        if count >= minimum {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("marker {marker} count did not reach {minimum}; last {count}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_marker_count(
    lease: &NodeLease<'_>,
    marker: &str,
    expected: i64,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let count = marker_count(lease, marker)?;
        if count == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("marker {marker} count did not reach {expected}; last {count}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Send `signal` to every container process whose argv carries `marker`, from a
/// process fully outside the command session (a container `python3` exec). This
/// is the "killed by another process" path: termination that did NOT come from
/// the `cancel` / Ctrl-C/Ctrl-D write_stdin API. The scanner excludes its own pid so
/// it never signals itself (its argv carries `marker` too).
fn kill_marker(lease: &NodeLease<'_>, marker: &str, signal: i32) -> Result<()> {
    let script = format!(
        r#"import os, pathlib
marker = {marker:?}
for proc in pathlib.Path("/proc").iterdir():
    if not proc.name.isdigit() or int(proc.name) == os.getpid():
        continue
    try:
        cmdline = proc.joinpath("cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "ignore")
    except OSError:
        continue
    if marker in cmdline:
        try:
            os.kill(int(proc.name), {signal})
        except OSError:
            pass
"#
    );
    lease.container().exec(&["python3", "-c", &script])?;
    Ok(())
}

/// Poll `collect_completed` until the session parks a terminal completion. A
/// fire-and-forget session (no live poller) finalizes through the reaper, so the
/// completion arrives asynchronously and must be polled for.
fn collect_completion(lease: &NodeLease<'_>, id: &str, within: Duration) -> Result<Value> {
    let deadline = Instant::now() + within;
    loop {
        let collected = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({ "command_session_ids": [id] }),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            return Ok(completion.clone());
        }
        if Instant::now() >= deadline {
            bail!("session {id} never parked a completion within {within:?}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// A process that died by signal surfaces a signal-coded exit: `runner.rs`
/// encodes it as a negative code (`-signal`), and a wrapping shell re-encodes the
/// same death as `128 + signal`. Either form distinguishes a kill from a clean or
/// ordinary nonzero exit.
fn signal_coded_exit(exit_code: i64) -> bool {
    !(0..128).contains(&exit_code)
}

#[test]
fn external_signal_kill_is_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = process_marker();
    // A separate container process SIGKILLs the foreground out from under the
    // session — no cancel API call is involved. The runner must reap the
    // signal death, finalize the session, park exactly one completion, and release
    // the lease.
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("bash -lc 'echo kill-ready; exec -a {marker} sleep 60'"),
            "yield_time_ms": 1000,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    assert!(stdout(&started).contains("kill-ready"), "{started}");
    let id = as_str(&started, "command_session_id")?.to_owned();
    wait_for_marker_at_least(&lease, &marker, 1)?;

    kill_marker(&lease, &marker, 9)?;

    let completion = collect_completion(&lease, &id, Duration::from_secs(10))?;
    let result = completion.get("result").context("completion result")?;
    assert_ne!(
        as_str(result, "status")?,
        "ok",
        "an externally killed session must not report ok: {completion}"
    );
    assert!(
        signal_coded_exit(as_i64(result, "exit_code")?),
        "external SIGKILL should surface a signal-coded exit_code: {completion}"
    );
    assert!(
        stdout(result).contains("kill-ready"),
        "external SIGKILL completion should preserve output captured before death: {completion}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    wait_for_active_leases(&lease, 0)?;
    wait_for_marker_count(&lease, &marker, 0, Duration::from_secs(3))?;
    Ok(())
}

#[test]
fn self_kill_reports_signal_exit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // The command kills its own process-group leader; termination is driven by the
    // process itself, not by the cancel API, but must still surface a signal-coded
    // terminal exit. Fast self-kill usually completes within the yield window, so
    // read the structured envelope with `call` rather than `call_ok`.
    let exec = lease.call(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo bye; kill -9 $$'",
            "yield_time_ms": 2000,
            "timeout_seconds": 30
        }),
    )?;
    if as_str(&exec, "status")? == "running" {
        let id = as_str(&exec, "command_session_id")?.to_owned();
        let completion = collect_completion(&lease, &id, Duration::from_secs(10))?;
        let result = completion.get("result").context("completion result")?;
        assert_ne!(as_str(result, "status")?, "ok", "{completion}");
        assert!(
            signal_coded_exit(as_i64(result, "exit_code")?),
            "self-kill should surface a signal-coded exit_code: {completion}"
        );
        wait_for_command_session_transcript_recycled(&lease, &id)?;
    } else {
        assert_ne!(
            as_str(&exec, "status")?,
            "ok",
            "a self-killed command must not report ok: {exec}"
        );
        assert!(
            signal_coded_exit(as_i64(&exec, "exit_code")?),
            "self-kill should surface a signal-coded exit_code: {exec}"
        );
    }
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn external_kill_of_foreground_keeps_group_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let fg = process_marker();
    let peer = format!("{}_peer", process_marker());
    // A foreground plus a same-pgid background peer. Killing ONLY the foreground by
    // external signal must NOT finalize the session: the pgid scope-wait keeps it
    // running until the surviving peer also exits. This is the intersection of
    // "killed by other process" and "remains running".
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!(
                "bash -lc 'bash -c \"exec -a {peer} sleep 60\" & echo group-ready; exec -a {fg} sleep 60'"
            ),
            "yield_time_ms": 1000,
            "timeout_seconds": 120
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    assert!(stdout(&started).contains("group-ready"), "{started}");
    let id = as_str(&started, "command_session_id")?.to_owned();
    wait_for_marker_at_least(&lease, &fg, 1)?;
    wait_for_marker_at_least(&lease, &peer, 1)?;

    kill_marker(&lease, &fg, 9)?;
    wait_for_marker_count(&lease, &fg, 0, Duration::from_secs(3))?;

    // Peer still alive keeps the pgid non-empty, so the session stays running and
    // does not finalize.
    let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
    assert_eq!(
        as_i64(&count, "count")?,
        1,
        "a surviving same-pgid peer must keep the session running: {count}"
    );
    let still = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({ "command_session_ids": [id.clone()] }),
    )?;
    assert!(
        array(&still, "completions")?.is_empty(),
        "session must not finalize while the peer lives: {still}"
    );

    // The peer now exits too, so the scope-wait empties and the session finalizes.
    kill_marker(&lease, &peer, 9)?;
    wait_for_marker_count(&lease, &peer, 0, Duration::from_secs(3))?;
    let completion = collect_completion(&lease, &id, Duration::from_secs(10))?;
    assert_eq!(completion["command_session_id"], json!(&id), "{completion}");
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn write_stdin_to_completed_session_is_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A session that finishes on its own and is left uncollected. A late
    // write_stdin against the finished id must return a structured terminal
    // envelope (not a hang or a running zombie), distinct from the not-found error
    // returned for an id that never existed.
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo quick; sleep 1'",
            "yield_time_ms": 100,
            "timeout_seconds": 30
        }),
    )?;
    let id = as_str(&started, "command_session_id")?.to_owned();
    // Count returning to zero means the session left the live registry (finished);
    // its completion is parked but uncollected.
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;

    let late = lease.call(
        ops::API_V1_WRITE_STDIN,
        json!({
            "command_session_id": &id,
            "chars": "late\n",
            "yield_time_ms": 200
        }),
    )?;
    assert!(
        matches!(as_str(&late, "status")?, "ok" | "error" | "cancelled"),
        "write_stdin to a finished session must return a structured terminal status: {late}"
    );

    // Drain the parked completion so a recycled container starts clean.
    lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({ "command_session_ids": [&id] }),
    )?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn ctrl_c_char_cancels_command_session() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    assert_teardown_control_reaps_marker_process(&lease, "ctrl-c", "\u{3}")
}

#[test]
fn model_shell_sees_masked_proc() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // The runner masks /proc from the model shell (security: hide the host
    // process list). The scope-wait reads a pre-mask /proc fd internally, but the
    // command itself must STILL see an empty /proc — this guards that the masking
    // fd stays CLOEXEC and is never inherited by the command.
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'printf \"procvisible=%s\\n\" \"$(ls /proc 2>/dev/null | grep -cE \"^[0-9]+$\")\"'",
            "yield_time_ms": 2000,
            "timeout_seconds": 30
        }),
    )?;
    let exec = settle_foreground_command(&lease, exec, Instant::now() + Duration::from_secs(30))?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    assert!(
        stdout(&exec).contains("procvisible=0"),
        "model shell must see an empty masked /proc (no host process list): {exec}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}
