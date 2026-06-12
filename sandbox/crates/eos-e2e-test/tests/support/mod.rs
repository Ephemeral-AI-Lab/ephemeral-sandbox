#![allow(dead_code)]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::{live_pool_with_config, NodeLease, NodePool};
use eos_operation::core::catalog;
use eos_sandbox_host::protocol::{decode_trace_sidecar_base64, take_trace_sidecar};
use eos_trace::{decode_trace_batch, TraceRecord};
use serde_json::{json, Value};

pub(crate) fn live_pool_or_skip() -> Result<Option<Arc<NodePool>>> {
    let Some(pool) = live_pool_with_config(crate::E2E_CONFIG)? else {
        eprintln!("skipping live eos-e2e-test; enable with `--features e2e`");
        return Ok(None);
    };
    Ok(Some(pool))
}

/// Poll `sandbox.checkpoint.layer_metrics` until `active_leases` settles at `expected`,
/// returning the metrics payload. Layer-lease accounting is asynchronous on the
/// release path, so callers must poll rather than read it instantaneously.
///
/// # Errors
/// Returns an error if the metrics op fails or `active_leases` never reaches
/// `expected` within the deadline.
pub(crate) fn wait_for_active_leases(lease: &NodeLease<'_>, expected: i64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let metrics = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
        if as_i64(&metrics, "active_leases")? == expected {
            return Ok(metrics);
        }
        if Instant::now() >= deadline {
            bail!("active_leases did not reach {expected}: {metrics}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Exit every open isolated workspace on this lease's daemon. Used at the start
/// of tests that enter isolated sessions so residue from a prior checkout on a
/// recycled container (e.g. a session leaked when an assertion panicked past its
/// cleanup) does not push past the global isolated-workspace cap. Drains via the
/// ungated `list_open` + `exit` ops (the `test_reset` hook needs a daemon env
/// flag the harness does not set). Best-effort: errors are ignored.
pub(crate) fn reset_isolated_workspaces(lease: &NodeLease<'_>) {
    let Ok(listing) = lease.call(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({})) else {
        return;
    };
    let callers: Vec<String> = listing
        .get("open_caller_ids")
        .and_then(Value::as_array)
        .map(|callers| {
            callers
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for caller_id in callers {
        let _ = lease.call(
            catalog::SANDBOX_ISOLATION_EXIT,
            json!({"caller_id": caller_id, "grace_s": 0.0}),
        );
    }
}

/// Poll `sandbox.command.count` until `count` settles at `expected`.
///
/// # Errors
/// Returns an error if the count op fails or never reaches `expected` within
/// the deadline.
pub(crate) fn wait_for_command_count(lease: &NodeLease<'_>, expected: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = lease.call_ok(catalog::SANDBOX_COMMAND_COUNT, json!({}))?;
        if as_i64(&count, "count")? == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("command_count did not reach {expected}: {count}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Finalize a foreground `exec_command` response to its terminal outcome.
///
/// Native runs of a quick command finish inside the yield window and return
/// status `"ok"` directly. Under x86-on-arm64 emulation the ns-runner spawn,
/// PTY setup, and (for python workers) interpreter startup can outlast the
/// 1s yield, so `exec_command` legitimately returns status `"running"` with a
/// `command_id`. This polls `read_progress` (which takes the process exit and
/// finalizes the run the moment its child exits) until the status is terminal,
/// then reconstructs the terminal-exec wire shape by stripping `command_id`
/// — exactly what `exec_command` does for a non-`running` status. The returned
/// value carries the full finalized payload (`exit_code`, `changed_paths`,
/// `timings`), so the caller's real assertions still hold post-finalization.
///
/// # Errors
/// Returns an error if `read_progress` fails or the run does not finalize before
/// `deadline`.
pub(crate) fn finalize_foreground_command(
    lease: &NodeLease<'_>,
    response: Value,
    deadline: Instant,
) -> Result<Value> {
    if as_str(&response, "status")? != "running" {
        return Ok(response);
    }
    let command_id = as_str(&response, "command_id")?.to_owned();
    loop {
        // `call` (not `call_ok`): a command that finalizes to a NON-zero exit
        // returns `success:false`, which is a valid terminal outcome here, not a
        // transport error. `call_ok` would reject it and break error-exit
        // finalization.
        let progress = lease.call(
            catalog::SANDBOX_COMMAND_POLL,
            json!({"command_id": &command_id, "last_n_lines": 50}),
        )?;
        // A `read_progress` finalizes with `publish_completion = false`
        // and removes the run, so a second poll would 404. Stop on the first
        // terminal status. The finalized response still carries the command id
        // (read_progress does not strip it); strip it here to match the
        // terminal-exec shape `assert_command_ok` expects.
        if as_str(&progress, "status")? != "running" {
            let mut progress = progress;
            if let Some(object) = progress.as_object_mut() {
                object.remove("command_id");
            }
            return Ok(progress);
        }
        if Instant::now() >= deadline {
            bail!("foreground command {command_id} did not finalize before deadline: {progress}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Poll `read_progress` for `command_id` until its transcript contains `needle`
/// (timestamp prefix stripped). Tolerates output slipping past the first yield
/// window under emulation while still REQUIRING the needle to appear — it bails
/// (fails the test) if the deadline passes without it.
///
/// # Errors
/// Returns an error if `read_progress` fails or `needle` never appears within
/// the deadline.
pub(crate) fn wait_for_command_stdout_contains(
    lease: &NodeLease<'_>,
    command_id: &str,
    needle: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let progress = lease.call_ok(
            catalog::SANDBOX_COMMAND_POLL,
            json!({"command_id": command_id, "last_n_lines": 50}),
        )?;
        if clean_stdout(&progress).contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("command stdout did not surface {needle:?} before deadline: {progress}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn container_path_exists(lease: &NodeLease<'_>, path: &str) -> Result<bool> {
    let script = format!(
        r#"import pathlib
print("true" if pathlib.Path({path:?}).exists() else "false")
"#
    );
    match lease.container().exec(&["python3", "-c", &script])?.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        output => bail!("unexpected path-exists probe output for {path}: {output:?}"),
    }
}

pub(crate) fn wait_for_container_path(
    lease: &NodeLease<'_>,
    path: &str,
    expected_exists: bool,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let exists = container_path_exists(lease, path)?;
        if exists == expected_exists {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("container path {path} existence did not reach {expected_exists}; last {exists}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn command_transcript_path(command_id: &str) -> String {
    format!("/eos/scratch/commands/{command_id}/transcript.log")
}

pub(crate) fn isolated_command_transcript_path(
    workspace_handle_id: &str,
    command_id: &str,
) -> String {
    format!("/eos/scratch/isolated/{workspace_handle_id}/commands/{command_id}/transcript.log")
}

pub(crate) fn command_transcript_logs(lease: &NodeLease<'_>) -> Result<Vec<String>> {
    let script = r#"import json
import pathlib

paths = []
for root in [pathlib.Path("/eos/scratch/commands"), pathlib.Path("/eos/scratch/isolated")]:
    if root.exists():
        paths.extend(str(path) for path in root.rglob("transcript.log"))
print(json.dumps(sorted(paths)))
"#;
    let output = lease.container().exec(&["python3", "-c", script])?;
    serde_json::from_str(output.trim())
        .with_context(|| format!("parse command transcript log paths from {output:?}"))
}

pub(crate) fn wait_for_command_transcript_recycled(
    lease: &NodeLease<'_>,
    command_id: &str,
) -> Result<()> {
    wait_for_container_path(
        lease,
        &command_transcript_path(command_id),
        false,
        Duration::from_secs(3),
    )
}

pub(crate) fn wait_for_isolated_command_transcript_recycled(
    lease: &NodeLease<'_>,
    workspace_handle_id: &str,
    command_id: &str,
) -> Result<()> {
    wait_for_container_path(
        lease,
        &isolated_command_transcript_path(workspace_handle_id, command_id),
        false,
        Duration::from_secs(3),
    )
}

/// Seed a multi-file base into the lowerdir layer stack and return the total
/// bytes written. The daemon caps one `write_file` payload at 2 MiB, so a large
/// workspace is built from many sub-cap files. Used by O(1)-disk tests that
/// grow workspace size while asserting the overlay upperdir stays delta-sized.
pub(crate) fn seed_base_files(
    lease: &NodeLease<'_>,
    dir: &str,
    file_count: usize,
    bytes_each: usize,
) -> Result<usize> {
    for index in 0..file_count {
        lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": format!("{dir}/base-{index}.txt"),
                "content": "x".repeat(bytes_each),
                "overwrite": true
            }),
        )?;
    }
    Ok(file_count * bytes_each)
}

pub(crate) fn trace_record(response: &Value) -> Result<TraceRecord> {
    let mut response = response.clone();
    let sidecar = take_trace_sidecar(&mut response)
        .with_context(|| format!("response missing trace sidecar: {response}"))?;
    let batch = decode_trace_batch(&sidecar).context("decode trace sidecar")?;
    let mut records = batch.records;
    if records.len() != 1 {
        bail!(
            "expected one trace record in response sidecar, got {}",
            records.len()
        );
    }
    Ok(records.remove(0))
}

pub(crate) fn trace_export_records(response: &Value) -> Result<Vec<TraceRecord>> {
    let Some(encoded) = response.get("trace_batch_base64").and_then(Value::as_str) else {
        if response.get("record_count").and_then(Value::as_i64) == Some(0) {
            return Ok(Vec::new());
        }
        bail!("trace export missing trace_batch_base64: {response}");
    };
    let bytes = decode_trace_sidecar_base64(encoded).context("decode trace export batch")?;
    Ok(decode_trace_batch(&bytes)
        .context("decode trace export protobuf")?
        .records)
}

pub(crate) fn has_trace_event(
    record: &TraceRecord,
    module: &str,
    name: &str,
    predicate: impl Fn(&Value) -> bool,
) -> bool {
    record.events.iter().any(|event| {
        event.module == module && event.name == name && predicate(&event.details.value)
    })
}

pub(crate) fn as_bool(value: &Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} missing or not bool in {value}"))
}

pub(crate) fn as_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("{key} missing or not i64 in {value}"))
}

pub(crate) fn as_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} missing or not string in {value}"))
}

pub(crate) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("{key} missing or not array in {value}"))
}

pub(crate) fn stdout(value: &Value) -> &str {
    value
        .get("output")
        .and_then(|output| output.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Command stdout with the per-line `[ISO-8601] ` transcript timestamp prefix
/// removed. The daemon's PTY reader prepends a wall-clock timestamp to every
/// transcript line (for `read_progress` monitoring), so a finalized command's
/// `output.stdout` carries those prefixes. Tests that assert on the command's
/// actual output strip the environment-dependent timestamp first; lines without
/// a timestamp prefix pass through unchanged.
pub(crate) fn clean_stdout(value: &Value) -> String {
    strip_transcript_timestamps(stdout(value))
}

/// Strip the `[ISO-8601] ` timestamp prefix from each line of `raw`. See
/// [`clean_stdout`].
pub(crate) fn strip_transcript_timestamps(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (index, line) in raw.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(strip_line_timestamp(line));
    }
    out
}

fn strip_line_timestamp(line: &str) -> &str {
    let Some(rest) = line.strip_prefix('[') else {
        return line;
    };
    let Some(close) = rest.find("] ") else {
        return line;
    };
    // Only strip a real timestamp (`YYYY-...`): require 4 leading digits + `-`
    // so genuine bracketed output is never mistaken for a transcript prefix.
    let stamp = rest.as_bytes();
    if close >= 5 && stamp[..4].iter().all(u8::is_ascii_digit) && stamp[4] == b'-' {
        &rest[close + 2..]
    } else {
        line
    }
}

pub(crate) fn conflict_reason(value: &Value) -> String {
    value
        .get("conflict")
        .and_then(|conflict| conflict.get("reason"))
        .and_then(Value::as_str)
        .or_else(|| value.get("conflict_reason").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn conflict_message(value: &Value) -> String {
    value
        .get("conflict")
        .and_then(|conflict| conflict.get("message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("conflict_reason").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}
