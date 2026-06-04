use anyhow::{Context, Result};
use eos_e2e_test::audit::section;
use eos_e2e_test::cas::looks_like_sha256;
use eos_protocol::ops;
use serde_json::json;

use crate::common::{as_bool, as_i64, as_str, live_pool_or_skip};

#[test]
fn commit_collapses_layers() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for index in 0..5 {
        lease.call_ok(
            ops::API_V1_WRITE_FILE,
            json!({"path": format!("commit/collapse-{index}.txt"), "content": "x\n", "overwrite": true}),
        )?;
    }
    let before = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert!(as_i64(&before, "manifest_depth")? > 1);
    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(as_bool(&commit, "success")?);
    let after = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&after, "manifest_depth")?,
        1,
        "commit should collapse the active manifest to the workspace base: {after}"
    );
    Ok(())
}

#[test]
fn commit_materializes_merged_view() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/materialized.txt", "content": "materialized\n", "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    lease.call_ok(
        ops::API_BUILD_WORKSPACE_BASE,
        json!({"workspace_root": lease.workspace_root(), "reset": true}),
    )?;
    let read = lease.call_ok(
        ops::API_V1_READ_FILE,
        json!({"path": "commit/materialized.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "materialized\n");
    Ok(())
}

#[test]
fn commit_version_monotonic() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/version.txt", "content": "v1\n", "overwrite": true}),
    )?;
    let first = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/version.txt", "content": "v2\n", "overwrite": true}),
    )?;
    let second = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    assert!(
        as_i64(&second, "manifest_version")? >= as_i64(&first, "manifest_version")?,
        "commit manifest versions should be monotonic: first={first} second={second}"
    );
    Ok(())
}

#[test]
fn commit_emits_audit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "commit/audit.txt", "content": "audit\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    let commit = lease.call_ok(
        ops::API_COMMIT_TO_WORKSPACE,
        json!({"workspace_root": lease.workspace_root()}),
    )?;
    audit.collect()?;
    let event = audit
        .first("layer_stack.commit_completed")
        .context("layer_stack.commit_completed audit event")?;
    let layer_stack = section(event, "layer_stack").context("layer_stack audit section")?;
    assert_eq!(
        layer_stack
            .get("manifest_version")
            .and_then(serde_json::Value::as_i64),
        commit
            .get("manifest_version")
            .and_then(serde_json::Value::as_i64),
        "commit audit should report response manifest version: {event}"
    );
    assert!(
        layer_stack
            .get("manifest_root_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(looks_like_sha256),
        "commit audit should report a CAS-shaped root hash: {event}"
    );
    Ok(())
}
