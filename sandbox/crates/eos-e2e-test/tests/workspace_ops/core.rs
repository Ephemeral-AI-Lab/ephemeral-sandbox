use anyhow::{Context, Result};
use eos_e2e_test::client::error_kind;
use eos_protocol::{
    models::{MAX_FILE_BYTES, MAX_READ_BYTES},
    ops,
};
use serde_json::{json, Value};

use crate::common::{array, as_bool, as_i64, as_str, conflict_message, live_pool_or_skip};

#[test]
fn write_read_roundtrip() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/roundtrip.txt", "content": "roundtrip\n", "overwrite": true}),
    )?;
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "tool/roundtrip.txt"}))?;
    assert!(as_bool(&read, "exists")?);
    assert_eq!(as_str(&read, "content")?, "roundtrip\n");
    assert_eq!(as_str(&read, "encoding")?, "utf-8");
    Ok(())
}

#[test]
fn write_publishes_changed_paths() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/changed.txt", "content": "changed\n", "overwrite": true}),
    )?;
    assert_eq!(as_str(&write, "status")?, "committed");
    assert_eq!(as_str(&write, "mutation_source")?, "api_write");
    assert!(
        array(&write, "changed_paths")?
            .iter()
            .any(|path| path.as_str() == Some("tool/changed.txt")),
        "write response should list the published path: {write}"
    );
    Ok(())
}

#[test]
fn edit_search_replace_applied() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/edit.txt", "content": "alpha beta\n", "overwrite": true}),
    )?;
    let edit = lease.call_ok(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "tool/edit.txt",
            "edits": [{"old_text": "alpha", "new_text": "omega", "replace_all": false}]
        }),
    )?;
    assert_eq!(as_i64(&edit, "applied_edits")?, 1);
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "tool/edit.txt"}))?;
    assert_eq!(as_str(&read, "content")?, "omega beta\n");
    Ok(())
}

#[test]
fn edit_replace_all() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/replace-all.txt", "content": "x x x\n", "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "tool/replace-all.txt",
            "edits": [{"old_text": "x", "new_text": "y", "replace_all": true}]
        }),
    )?;
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "tool/replace-all.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "y y y\n");
    Ok(())
}

#[test]
fn edit_anchor_not_found() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/not-found.txt", "content": "present\n", "overwrite": true}),
    )?;
    let edit = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "tool/not-found.txt",
            "edits": [{"old_text": "absent", "new_text": "x", "replace_all": false}]
        }),
    )?;
    assert!(
        conflict_message(&edit).contains("anchor not found"),
        "missing anchor should surface the edit error catalog: {edit}"
    );
    Ok(())
}

#[test]
fn edit_count_mismatch() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/count-mismatch.txt", "content": "dup dup\n", "overwrite": true}),
    )?;
    let edit = lease.call(
        ops::API_V1_EDIT_FILE,
        json!({
            "path": "tool/count-mismatch.txt",
            "edits": [{"old_text": "dup", "new_text": "x", "replace_all": false}]
        }),
    )?;
    assert!(
        conflict_message(&edit).contains("count mismatch"),
        "ambiguous anchor should surface the edit error catalog: {edit}"
    );
    Ok(())
}

#[test]
fn read_nonexistent() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "tool/missing.txt"}))?;
    assert!(!as_bool(&read, "exists")?);
    assert_eq!(as_str(&read, "content")?, "");
    Ok(())
}

#[test]
fn glob_matches() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/glob/a.rs", "content": "fn a() {}\n", "overwrite": true}),
    )?;
    let glob = lease.call_ok(ops::API_V1_GLOB, json!({"pattern": "tool/glob/*.rs"}))?;
    assert_eq!(as_i64(&glob, "num_files")?, 1);
    assert_eq!(
        array(&glob, "filenames")?[0],
        Value::String("tool/glob/a.rs".to_owned())
    );
    Ok(())
}

#[test]
fn grep_content_mode() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "tool/grep.txt", "content": "needle\nhay\n", "overwrite": true}),
    )?;
    let grep = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "tool", "output_mode": "content"}),
    )?;
    assert!(as_str(&grep, "content")?.contains("needle"));
    Ok(())
}

#[test]
fn read_max_bytes_guard() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("mkdir -p tool && python3 - <<'PY'\nopen('tool/too-big-read.txt', 'wb').write(b'x' * {})\nPY", MAX_READ_BYTES + 1),
            "yield_time_ms": 1000,
            "timeout_seconds": 20,
            "max_output_tokens": 1000
        }),
    )?;
    assert_eq!(
        as_str(&exec, "status")?,
        "ok",
        "seed command should publish big file: {exec}"
    );
    let read = lease.call(
        ops::API_V1_READ_FILE,
        json!({"path": "tool/too-big-read.txt"}),
    )?;
    assert_eq!(error_kind(&read), Some("invalid_envelope"));
    assert!(
        read.get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .context("error message")?
            .contains("file too large"),
        "large read should fail with the read guard: {read}"
    );
    Ok(())
}

#[test]
fn write_max_file_bytes_guard() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let write = lease.call(
        ops::API_V1_WRITE_FILE,
        json!({
            "path": "tool/too-big-write.txt",
            "content": "x".repeat(MAX_FILE_BYTES + 1),
            "overwrite": true
        }),
    )?;
    assert_eq!(error_kind(&write), Some("invalid_envelope"));
    assert!(
        write
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .context("error message")?
            .contains("file too large"),
        "large write should fail before OCC publish: {write}"
    );
    Ok(())
}
