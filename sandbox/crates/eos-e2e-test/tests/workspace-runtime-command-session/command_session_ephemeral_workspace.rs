//! Background-PTY command-session semantics (spec point 5).
//!
//! The session exit condition is process-GROUP based: a foreground command that
//! backgrounds a same-pgid child stays RUNNING until ALL members exit (a fresh-ns
//! exec is NEWUSER|NEWNS only, so the runner scope-waits on the whole group). A
//! command-session cancel kills the entire group, and no descendant is left
//! behind. All children use BOUNDED sleeps + unique markers so any
//! early-return leak self-heals and never collides with another test.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::{unique_suffix, NodeLease};
use eos_operation::core::catalog;
use serde_json::json;

use crate::support::{
    array, as_i64, as_str, finalize_foreground_command, live_pool_or_skip, stdout,
    wait_for_active_leases, wait_for_command_session_transcript_recycled, wait_for_session_count,
};

#[test]
fn exec_simple() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({"cmd": "true", "yield_time_ms": 1000, "timeout_seconds": 5}),
    )?;
    let exec = finalize_foreground_command(&lease, exec, Instant::now() + Duration::from_secs(15))?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);
    assert_eq!(stdout(&exec), "");
    Ok(())
}

#[test]
fn lingering_child_keeps_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // The foreground finishes (prints "done") but backgrounds a same-pgid child:
    // the session must stay running and uncollectable while the child lives.
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo up; sleep 30 & echo done'",
            "yield_time_ms": 1000,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(
        as_str(&exec, "status")?,
        "running",
        "a lingering background child must keep the session running: {exec}"
    );
    assert!(
        stdout(&exec).contains("done"),
        "the foreground must have completed before yield: {exec}"
    );
    let id = as_str(&exec, "command_id")?.to_owned();

    let count = lease.call_ok(catalog::SANDBOX_COMMAND_COUNT, json!({}))?;
    assert_eq!(as_i64(&count, "count")?, 1, "session must be live: {count}");

    let collected = lease.call_ok(
        catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
        json!({"command_ids": [id.clone()]}),
    )?;
    assert!(
        array(&collected, "completions")?.is_empty(),
        "session must not be finalized while the child lives: {collected}"
    );

    cancel(&lease, &id)?;
    Ok(())
}

#[test]
fn session_completes_only_after_all_subprocesses_exit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'sleep 3 & echo started'",
            "yield_time_ms": 800,
            "timeout_seconds": 60,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    let id = as_str(&exec, "command_id")?.to_owned();

    // The completion must NOT arrive before the 3s child exits; it does once the
    // whole process group is gone (exit condition = all subprocesses complete).
    let deadline = Instant::now() + Duration::from_secs(8);
    let completion = loop {
        let collected = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({"command_ids": [id.clone()]}),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            break completion.clone();
        }
        if Instant::now() >= deadline {
            cancel(&lease, &id)?;
            bail!("session never completed after the background child exited");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(completion["command_id"], json!(&id));
    assert_eq!(
        completion
            .get("result")
            .and_then(|result| result.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("ok"),
        "completion must report ok once all subprocesses exited: {completion}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    Ok(())
}

#[test]
fn cancel_kills_whole_session() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A stdin reader PLUS a same-pgid background sleeper: cancel must kill the
    // entire group, not just the foreground reader.
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'sleep 60 & python3 -u -c \"import sys; print(\\\"ready\\\", flush=True); sys.stdin.readline(); import time; time.sleep(60)\"'",
            "yield_time_ms": 1500,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    let id = as_str(&exec, "command_id")?.to_owned();

    // Cancel kills the whole session, so its hardened outcome is
    // success:false; use `call` to read the terminal response, not `call_ok`.
    let cancelled = lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": &id}))?;
    assert!(
        matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
        "cancel must drive the session to a terminal status: {cancelled}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    Ok(())
}

#[test]
fn cancel_reaps_lingering_descendant() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = format!("eos_e2e_orphan_{}", unique_suffix().replace('-', "_"));
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!("bash -lc 'bash -c \"exec -a {marker} sleep 60\" & echo descendant-ready; wait'"),
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    let id = as_str(&exec, "command_id")?.to_owned();
    assert!(
        marker_count(&lease, &marker)? > 0,
        "the descendant must be alive before cancel"
    );

    cancel(&lease, &id)?;
    // The group-targeted cancel must reap the same-pgid descendant: no orphan.
    wait_for_marker_count(&lease, &marker, 0)?;
    Ok(())
}

fn cancel(lease: &NodeLease<'_>, id: &str) -> Result<()> {
    lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": id}))?;
    wait_for_command_session_transcript_recycled(lease, id)?;
    Ok(())
}

/// Count container processes whose argv contains `marker`, scanning `/proc` from
/// the host PID namespace (where a reparented orphan would still be visible).
/// Runs `python3` directly (no shell wrapper) and excludes its own pid, so the
/// scanner — which itself carries `marker` in argv — never self-counts.
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

fn wait_for_marker_count(lease: &NodeLease<'_>, marker: &str, expected: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let count = marker_count(lease, marker)?;
        if count == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("marker {marker} count did not reach {expected}; last {count}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Signal every container process whose argv carries `marker`, from a process
/// outside the session (a container `python3` exec), excluding the scanner's own
/// pid. Used to clean up an intentionally-leaked escaped descendant.
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

#[test]
fn live_background_emitter_keeps_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // The foreground exits after backgrounding a same-pgid child that KEEPS
    // emitting. The session must stay running, and read_progress must surface
    // output progressively from the timestamped transcript.
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo fg-start; (for i in $(seq 1 12); do echo tick-$i; sleep 0.3; done) & echo fg-done'",
            "yield_time_ms": 800,
            "timeout_seconds": 60,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    assert!(
        stdout(&exec).contains("fg-done"),
        "foreground must finish before yield: {exec}"
    );
    let id = as_str(&exec, "command_id")?.to_owned();

    let mut seen_max = 0;
    let deadline = Instant::now() + Duration::from_secs(8);
    while seen_max < 6 && Instant::now() < deadline {
        let poll = lease.call_ok(
            catalog::SANDBOX_COMMAND_POLL,
            json!({
                "command_id": &id,
                "last_n_lines": 8,
            }),
        )?;
        let out = stdout(&poll);
        for tick in 1..=12 {
            if out.contains(&format!("tick-{tick}")) {
                seen_max = seen_max.max(tick);
            }
        }
    }
    assert!(
        seen_max >= 6,
        "read_progress should surface progressive background output; reached tick {seen_max}"
    );

    let cdeadline = Instant::now() + Duration::from_secs(8);
    let completion = loop {
        let collected = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({ "command_ids": [id.clone()] }),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            break completion.clone();
        }
        if Instant::now() >= cdeadline {
            cancel(&lease, &id)?;
            bail!("live background emitter session never completed");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(completion["command_id"], json!(&id), "{completion}");
    wait_for_session_count(&lease, 0)?;
    wait_for_command_session_transcript_recycled(&lease, &id)?;
    Ok(())
}

#[test]
fn running_stderr_only_emitter_is_visible() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // "Returns a stderr but remains running": a never-exiting process that writes
    // ONLY to stderr. The PTY merges stderr into stdout, so the text must be
    // visible in output.stdout while status == running, and the structured stderr
    // field stays empty even for a non-exiting session.
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"err-only-line\", file=sys.stderr, flush=True); time.sleep(60)'",
            "yield_time_ms": 1000,
            "timeout_seconds": 120,}),
    )?;
    assert_eq!(
        as_str(&exec, "status")?,
        "running",
        "a stderr-only emitter must stay running: {exec}"
    );
    assert!(
        stdout(&exec).contains("err-only-line"),
        "merged PTY must surface stderr in output.stdout: {exec}"
    );
    let structured_stderr = exec
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        structured_stderr.is_empty(),
        "merged PTY keeps the structured stderr field empty: {exec}"
    );
    let id = as_str(&exec, "command_id")?.to_owned();
    cancel(&lease, &id)?;
    wait_for_session_count(&lease, 0)?;
    Ok(())
}

#[test]
fn setsid_descendant_escapes_and_leaks_in_ephemeral() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = format!("eos_e2e_escape_{}", unique_suffix().replace('-', "_"));
    // A `setsid` child gets a NEW process group, so the pgid scope-wait cannot see
    // it: the session COMPLETES immediately (unlike a same-pgid nohup/`&` child).
    // The ephemeral path reaps by pgid only (no PID-ns, no cgroup backstop), so the
    // descendant LEAKS past session completion and lease release. This pins that
    // contract; the isolated counterpart proves the cgroup-backed mode reaps it. A
    // self-healing `sleep 30` keeps CI from accumulating ghosts, and a future
    // teardown backstop would flip the post-release marker assertion to 0.
    let completed = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "bash -lc 'setsid bash -c \"exec -a {marker} sleep 30\" >/dev/null 2>&1 & echo escaped-ready'"
            ),
            "yield_time_ms": 1500,
            "timeout_seconds": 60,}),
    )?;
    assert_eq!(
        as_str(&completed, "status")?,
        "ok",
        "a setsid child escapes the pgid, so the session completes: {completed}"
    );
    assert!(
        completed.get("command_id").is_none(),
        "a completed escaped-child command must not leave a session handle: {completed}"
    );
    assert!(stdout(&completed).contains("escaped-ready"), "{completed}");
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    // The escaped descendant is still alive AFTER the lease released: ephemeral mode
    // reaps by pgid only and never killpg'd this escaped group.
    assert!(
        marker_count(&lease, &marker)? >= 1,
        "ephemeral mode leaks the escaped setsid descendant past lease release"
    );
    kill_marker(&lease, &marker, 9)?;
    wait_for_marker_count(&lease, &marker, 0)?;
    Ok(())
}

#[test]
fn nonsetsid_detach_vectors_stay_tracked() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // "Other cases": disown and subshell-background do NOT change the process
    // group, so — unlike setsid — they stay tracked and keep the session running.
    // pgid membership, not the detach idiom, is the tracking boundary.
    for (label, cmd) in [
        ("disown", "bash -lc 'sleep 30 & disown; echo disowned'"),
        ("subshell", "bash -lc '( sleep 30 & ); echo subshelled'"),
    ] {
        let exec = lease.call_ok(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": cmd,
                "yield_time_ms": 800,
                "timeout_seconds": 60,}),
        )?;
        assert_eq!(
            as_str(&exec, "status")?,
            "running",
            "{label}: a same-pgid background child must keep the session running: {exec}"
        );
        let id = as_str(&exec, "command_id")?.to_owned();
        let collected = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({ "command_ids": [id.clone()] }),
        )?;
        assert!(
            array(&collected, "completions")?.is_empty(),
            "{label}: session must not finalize while the same-pgid child lives: {collected}"
        );
        cancel(&lease, &id)?;
        wait_for_session_count(&lease, 0)?;
    }
    Ok(())
}
