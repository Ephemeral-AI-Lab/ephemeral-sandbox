use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use eos_e2e_test::NodeLease;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_i64, as_str, clean_stdout, live_pool_or_skip, stdout, wait_for_active_leases,
    wait_for_command_session_transcript_recycled, wait_for_session_count,
};

struct MarkerSession {
    id: String,
    marker: String,
}

#[test]
fn external_sigterm_child_finalizes_via_collect_completed() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let session = start_waiting_marker_child(&lease, "external_child")?;

    let body = (|| -> Result<()> {
        wait_for_marker_at_least(&lease, &session.marker, 1)?;
        signal_marker(&lease, &session.marker, "TERM", SignalTarget::Pid)?;
        wait_for_marker_count(&lease, &session.marker, 0, Duration::from_secs(10))?;

        let completion = wait_for_completion(&lease, &session.id, Duration::from_secs(15))?;
        let result = completion.get("result").context("completion result")?;
        ensure!(
            as_str(result, "status")? == "error",
            "externally terminated child should finalize as error: {completion}"
        );
        ensure!(
            as_i64(result, "exit_code")? != 0,
            "externally terminated child should preserve a nonzero exit code: {completion}"
        );
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_session_transcript_recycled(&lease, &session.id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = cancel_session(&lease, &session.id);
    }
    body
}

#[test]
fn external_sigkill_process_group_is_observed_by_read_progress() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let session = start_waiting_marker_child(&lease, "external_pgid")?;

    let body = (|| -> Result<()> {
        wait_for_marker_at_least(&lease, &session.marker, 1)?;
        signal_marker(&lease, &session.marker, "KILL", SignalTarget::ProcessGroup)?;
        wait_for_marker_count(&lease, &session.marker, 0, Duration::from_secs(10))?;

        let terminal =
            wait_for_read_progress_terminal(&lease, &session.id, Duration::from_secs(15))?;
        ensure!(
            matches!(as_str(&terminal, "status")?, "error" | "cancelled"),
            "read_progress should observe externally killed process group as terminal: {terminal}"
        );
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_session_transcript_recycled(&lease, &session.id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = cancel_session(&lease, &session.id);
    }
    body
}

#[test]
fn silent_redirected_subprocess_keeps_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let session = start_silent_marker_child(&lease)?;

    let body = (|| -> Result<()> {
        wait_for_marker_at_least(&lease, &session.marker, 1)?;
        let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
        ensure!(
            as_i64(&count, "count")? == 1,
            "silent same-pgid subprocess should keep the session live: {count}"
        );

        let not_done = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [session.id.clone()]}),
        )?;
        ensure!(
            array(&not_done, "completions")?.is_empty(),
            "silent same-pgid subprocess must not be finalized early: {not_done}"
        );

        let progress = lease.call_ok(
            ops::API_V1_COMMAND_READ_PROGRESS,
            json!({
                "command_session_id": &session.id,
                "last_n_lines": 4,
            }),
        )?;
        ensure!(
            as_str(&progress, "status")? == "running",
            "read_progress should still see the silent subprocess running: {progress}"
        );
        ensure!(
            stdout(&progress).contains("silent-parent-done"),
            "read_progress should expose the transcript tail for the running session: {progress}"
        );

        cancel_session(&lease, &session.id)?;
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_marker_count(&lease, &session.marker, 0, Duration::from_secs(10))?;
        Ok(())
    })();

    if body.is_err() {
        let _ = cancel_session(&lease, &session.id);
    }
    body
}

fn start_waiting_marker_child(lease: &NodeLease<'_>, label: &str) -> Result<MarkerSession> {
    let cmd = format!(
        "python3 -u - <<'PY'\n\
import os, subprocess, sys, time\n\
marker = f\"eos_e2e_{label}_{{os.getpid()}}_{{time.time_ns()}}\"\n\
print(\"marker:\" + marker, flush=True)\n\
child = subprocess.Popen([\"bash\", \"-c\", \"exec -a \\\"$0\\\" sleep 60\", marker])\n\
print(\"child-ready\", flush=True)\n\
rc = child.wait()\n\
sys.exit(128 + (-rc) if rc < 0 else rc)\n\
PY"
    );
    start_marker_session(lease, cmd, "child-ready")
}

fn start_silent_marker_child(lease: &NodeLease<'_>) -> Result<MarkerSession> {
    let cmd = concat!(
        "python3 -u - <<'PY'\n",
        "import os, subprocess, time\n",
        "marker = f\"eos_e2e_silent_child_{os.getpid()}_{time.time_ns()}\"\n",
        "print(\"marker:\" + marker, flush=True)\n",
        "subprocess.Popen(\n",
        "    [\"bash\", \"-c\", \"exec -a \\\"$0\\\" sleep 60\", marker],\n",
        "    stdin=subprocess.DEVNULL,\n",
        "    stdout=subprocess.DEVNULL,\n",
        "    stderr=subprocess.DEVNULL,\n",
        ")\n",
        "print(\"silent-parent-done\", flush=True)\n",
        "PY"
    );
    start_marker_session(lease, cmd, "silent-parent-done")
}

fn start_marker_session(
    lease: &NodeLease<'_>,
    cmd: impl Into<String>,
    readiness: &str,
) -> Result<MarkerSession> {
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": cmd.into(),
            "yield_time_ms": 700,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "marker command should stay running: {started}"
    );
    let id = as_str(&started, "command_session_id")?.to_owned();
    let marker = wait_for_stdout_marker(lease, &id, &started)?;
    wait_for_session_stdout_contains(lease, &id, &started, readiness)?;
    Ok(MarkerSession { id, marker })
}

fn wait_for_stdout_marker(
    lease: &NodeLease<'_>,
    session_id: &str,
    initial: &Value,
) -> Result<String> {
    // Slow python/ns-runner startup under x86-on-arm64 emulation can delay the
    // first transcript flush well past the native window.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = initial.clone();
    loop {
        if let Some(marker) = marker_from_stdout(&last) {
            return Ok(marker);
        }
        if Instant::now() >= deadline {
            bail!("session output never contained generated marker: {last}");
        }
        last = poll_session_output(lease, session_id, 250)?;
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_session_stdout_contains(
    lease: &NodeLease<'_>,
    session_id: &str,
    initial: &Value,
    needle: &str,
) -> Result<()> {
    if stdout(initial).contains(needle) {
        return Ok(());
    }
    // Readiness line can lag behind slow emulated python startup.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = initial.clone();
    loop {
        if Instant::now() >= deadline {
            bail!("session output never contained {needle}: {last}");
        }
        let poll = poll_session_output(lease, session_id, 250)?;
        if stdout(&poll).contains(needle) {
            return Ok(());
        }
        last = poll;
        thread::sleep(Duration::from_millis(50));
    }
}

fn poll_session_output(
    lease: &NodeLease<'_>,
    session_id: &str,
    last_n_lines: u64,
) -> Result<Value> {
    lease.call_ok(
        ops::API_V1_COMMAND_READ_PROGRESS,
        json!({
            "command_session_id": session_id,
            "last_n_lines": last_n_lines,
        }),
    )
}

fn marker_from_stdout(value: &Value) -> Option<String> {
    // The daemon transcript prefixes every line with `[ISO-8601] `, so the
    // marker line is `[..] marker:...`. Strip the timestamp before the anchored
    // `marker:` match; otherwise extraction never succeeds (format, not timing).
    let normalized = clean_stdout(value).replace('\r', "");
    normalized.lines().find_map(|line| {
        line.strip_prefix("marker:")
            .map(str::trim)
            .filter(|marker| !marker.is_empty())
            .map(ToOwned::to_owned)
    })
}

#[derive(Clone, Copy)]
enum SignalTarget {
    Pid,
    ProcessGroup,
}

impl SignalTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::ProcessGroup => "pgid",
        }
    }
}

fn signal_marker(
    lease: &NodeLease<'_>,
    marker: &str,
    signal: &str,
    target: SignalTarget,
) -> Result<()> {
    let script = format!(
        r#"import os, pathlib, signal, sys
marker = {marker:?}
signum = getattr(signal, "SIG{signal}")
target = {target:?}
matches = []
for proc in pathlib.Path("/proc").iterdir():
    if not proc.name.isdigit():
        continue
    pid = int(proc.name)
    if pid == os.getpid():
        continue
    try:
        cmdline = proc.joinpath("cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "ignore")
    except OSError:
        continue
    if marker in cmdline:
        try:
            matches.append((pid, os.getpgid(pid)))
        except ProcessLookupError:
            pass
if not matches:
    raise SystemExit("marker_not_found")
sent = set()
for pid, pgid in matches:
    try:
        if target == "pgid":
            if pgid in sent:
                continue
            os.killpg(pgid, signum)
            sent.add(pgid)
        else:
            os.kill(pid, signum)
            sent.add(pid)
    except ProcessLookupError:
        pass
print(len(sent))
"#,
        target = target.as_str()
    );
    let output = lease.container().exec(&["python3", "-c", &script])?;
    ensure!(
        output.trim().parse::<usize>().unwrap_or_default() > 0,
        "signal script did not report a target for marker {marker}: {output:?}"
    );
    Ok(())
}

fn wait_for_completion(
    lease: &NodeLease<'_>,
    session_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let collected = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [session_id]}),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            return Ok(completion.clone());
        }
        if Instant::now() >= deadline {
            bail!("session completion was not parked before deadline: {collected}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_read_progress_terminal(
    lease: &NodeLease<'_>,
    session_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        let response = lease.call(
            ops::API_V1_COMMAND_READ_PROGRESS,
            json!({
                "command_session_id": session_id,
                "last_n_lines": 8,
            }),
        )?;
        if as_str(&response, "status").unwrap_or_default() != "running" {
            return Ok(response);
        }
        if Instant::now() >= deadline {
            bail!("read_progress never observed terminal state; last: {last:?}");
        }
        last = Some(response);
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
    // Slow emulated spawn delays the child appearing under /proc.
    let deadline = Instant::now() + Duration::from_secs(15);
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

fn cancel_session(lease: &NodeLease<'_>, id: &str) -> Result<Value> {
    let cancelled = lease.call(
        ops::API_V1_COMMAND_CANCEL,
        json!({"command_session_id": id}),
    )?;
    wait_for_command_session_transcript_recycled(lease, id)?;
    Ok(cancelled)
}
