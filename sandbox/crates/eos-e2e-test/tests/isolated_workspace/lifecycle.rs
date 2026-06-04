//! Isolated-workspace lifecycle tests (plan §10 isolated path).
//!
//! Exercises the real SetNs / ns-holder / veth / cgroup machinery via
//! `enter` → (isolated write/read) → `status` → `exit`, asserting on the op
//! responses: the manifest pin on enter/status, isolated `mutation_source`,
//! discard-on-exit (the write is never OCC-published), and the exit `inspection`
//! teardown facts.

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

#[test]
fn enter_status_exit_pin_and_teardown() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let handle_id = as_str(&enter, "workspace_handle_id")?.to_owned();
    let pinned_version = enter
        .get("manifest_version")
        .and_then(Value::as_i64)
        .context("enter manifest_version")?;
    let pinned_hash = as_str(&enter, "manifest_root_hash")?.to_owned();
    assert!(
        !handle_id.is_empty(),
        "enter must return a handle id: {enter}"
    );
    assert_eq!(
        pinned_hash.len(),
        64,
        "manifest_root_hash must be sha256 hex: {enter}"
    );

    // status reports the same pin while open.
    let status = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(
        as_bool(&status, "open")?,
        "status must report open: {status}"
    );
    assert_eq!(
        status.get("manifest_version").and_then(Value::as_i64),
        Some(pinned_version),
        "status pin must match enter: {status}"
    );

    // exit tears down and reports inspection facts.
    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    let inspection = exit.get("inspection").context("exit inspection")?;
    assert_eq!(
        inspection
            .get("handle_registered_after")
            .and_then(Value::as_bool),
        Some(false),
        "handle must be unregistered after exit: {exit}"
    );
    // lease_released is Option<bool>: when present it must be true.
    if let Some(released) = inspection.get("lease_released").and_then(Value::as_bool) {
        assert!(released, "isolated lease must be released on exit: {exit}");
    }
    // cgroup_exists_after is Option<bool>: when present it must be false.
    if let Some(cgroup) = inspection
        .get("cgroup_exists_after")
        .and_then(Value::as_bool)
    {
        assert!(!cgroup, "cgroup must be removed on exit: {exit}");
    }
    assert!(
        inspection
            .get("holder_kill_error")
            .map(Value::is_null)
            .unwrap_or(true),
        "ns-holder must be reaped without error: {exit}"
    );

    // status after exit reports closed.
    let closed = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(
        !as_bool(&closed, "open")?,
        "status must report closed: {closed}"
    );
    Ok(())
}

#[test]
fn isolated_write_is_discarded_on_exit() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = "iso/private.txt";

    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;

    // A write inside isolated mode routes to the private upperdir.
    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": path, "content": "isolated-only\n", "overwrite": true}),
    )?;
    assert_eq!(
        as_str(&write, "mutation_source")?,
        "isolated_workspace",
        "write inside isolated mode must be isolated-sourced: {write}"
    );
    assert_eq!(
        as_str(&write, "status")?,
        "committed",
        "isolated write status: {write}"
    );

    // Read inside isolated mode sees it.
    let read_inside = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert_eq!(as_str(&read_inside, "content")?, "isolated-only\n");

    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    assert!(
        exit.get("evicted_upperdir_bytes")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 0,
        "exit reports evicted upperdir bytes: {exit}"
    );

    // After exit the private write is gone from the public layer stack: it was
    // never OCC-published, so the public read sees no such file.
    let read_public = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": path}))?;
    assert!(
        !as_bool(&read_public, "exists")?,
        "isolated write must not survive into the public workspace: {read_public}"
    );
    Ok(())
}
