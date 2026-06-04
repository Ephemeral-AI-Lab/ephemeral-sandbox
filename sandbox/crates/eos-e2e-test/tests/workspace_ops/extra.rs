//! Tool-call edge cases / error catalog (plan §6 tool_calls tier).
//!
//! Fills the deterministic response-oracle gaps the broad smoke test does not
//! cover: missing-file reads, the edit error catalog (anchor-not-found /
//! count-mismatch), create-only write conflicts, glob limit truncation, and the
//! grep output modes. All assert purely on the op response payload.

use std::sync::Arc;

use anyhow::{Context, Result};
use eos_e2e_test::{live_pool, NodePool};
use eos_protocol::ops;
use serde_json::{json, Value};

fn live_pool_or_skip() -> Result<Option<Arc<NodePool>>> {
    let Some(pool) = live_pool()? else {
        eprintln!("skipping live eos-e2e-test; enable with `--features e2e`");
        return Ok(None);
    };
    Ok(Some(pool))
}

fn as_bool(value: &Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} missing or not bool in {value}"))
}

fn as_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} missing or not string in {value}"))
}

/// The human-readable conflict message a guarded result carries, if any.
fn conflict_message(value: &Value) -> String {
    value
        .get("conflict")
        .and_then(|c| c.get("message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("conflict_reason").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

#[test]
fn read_nonexistent_reports_absent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "does/not/exist.txt"}))?;
    assert!(
        !as_bool(&read, "exists")?,
        "missing file must report exists=false: {read}"
    );
    Ok(())
}

#[test]
fn edit_error_catalog_anchor_not_found_and_count_mismatch() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    // anchor-not-found
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "edit/a.txt", "content": "hello world\n", "overwrite": true}),
    )?;
    let not_found = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({"path": "edit/a.txt", "edits": [{"old_text": "ABSENT", "new_text": "x", "replace_all": false}]}),
    )?;
    assert!(
        conflict_message(&not_found).contains("anchor not found"),
        "missing anchor must surface NotFound: {not_found}"
    );

    // count-mismatch (anchor appears twice, replace_all=false)
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "edit/b.txt", "content": "dup dup\n", "overwrite": true}),
    )?;
    let mismatch = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({"path": "edit/b.txt", "edits": [{"old_text": "dup", "new_text": "x", "replace_all": false}]}),
    )?;
    assert!(
        conflict_message(&mismatch).contains("count mismatch"),
        "ambiguous anchor must surface CountMismatch: {mismatch}"
    );
    Ok(())
}

#[test]
fn write_create_only_conflict() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "co/only.txt", "content": "first\n", "overwrite": true}),
    )?;
    let rejected = lease.call(
        ops::API_V1_WRITE_FILE,
        json!({"path": "co/only.txt", "content": "second\n", "overwrite": false}),
    )?;
    let reason = rejected
        .get("conflict")
        .and_then(|c| c.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        reason, "create_only_existing",
        "overwrite=false on an existing file must be a create-only conflict: {rejected}"
    );
    Ok(())
}

#[test]
fn glob_limit_truncation() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Create 120 files (> DEFAULT_GLOB_LIMIT=100) in one overlay exec.
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "mkdir -p many && for i in $(seq 1 120); do : > many/f$i.txt; done",
            "yield_time_ms": 2000,
            "timeout_seconds": 30
        }),
    )?;
    assert_eq!(
        as_str(&exec, "status")?,
        "ok",
        "seed exec must succeed: {exec}"
    );

    let glob = lease.call_ok(ops::API_V1_GLOB, json!({"pattern": "many/*.txt"}))?;
    assert!(
        as_bool(&glob, "truncated")?,
        "120 matches must truncate at the glob limit: {glob}"
    );
    let names = glob
        .get("filenames")
        .and_then(Value::as_array)
        .context("glob filenames")?;
    assert!(
        names.len() <= 100,
        "truncated glob must not exceed the limit: {}",
        names.len()
    );
    Ok(())
}

#[test]
fn grep_output_modes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "g/a.txt", "content": "needle here\nplain line\n", "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "g/b.txt", "content": "another needle\n", "overwrite": true}),
    )?;

    let files = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "g", "output_mode": "files_with_matches"}),
    )?;
    let names = files
        .get("filenames")
        .and_then(Value::as_array)
        .context("grep filenames")?;
    assert!(names.len() >= 2, "both files should match needle: {files}");

    let count = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "g", "output_mode": "count"}),
    )?;
    assert!(
        count
            .get("num_matches")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 2,
        "count mode must report >=2 matches: {count}"
    );
    Ok(())
}
