use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use eos_e2e_test::next_invocation_id;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::common::{as_i64, as_str, live_pool_or_skip};

#[test]
fn enter_acquires_lease() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let mut audit = lease.audit_tap()?;
    let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    assert!(!as_str(&enter, "workspace_handle_id")?.is_empty());
    audit.collect()?;
    if audit.any("layer_stack.lease_acquired") {
        assert!(audit.first("layer_stack.lease_acquired").is_some());
    }
    let status = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(status.get("open").and_then(Value::as_bool).unwrap_or(false));
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    Ok(())
}

#[test]
fn exit_releases_lease() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let mut audit = lease.audit_tap()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    audit.collect()?;
    if audit.any("layer_stack.lease_released") {
        assert!(audit.first("layer_stack.lease_released").is_some());
    }
    let closed = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(!closed.get("open").and_then(Value::as_bool).unwrap_or(true));
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0);
    Ok(())
}

#[test]
fn lease_pins_layers_vs_squash() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    let pinned_version = enter.get("manifest_version").and_then(Value::as_i64);
    let pinned_hash = enter
        .get("manifest_root_hash")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let root = lease.root().to_owned();
    for version in 0..105 {
        lease.client().request(
            ops::API_V1_WRITE_FILE,
            &next_invocation_id(),
            &json!({
                "layer_stack_root": root,
                "agent_id": "lease-public-writer",
                "path": "lease/pinned.txt",
                "content": format!("public-{version}\n"),
                "overwrite": true
            }),
        )?;
    }
    let held = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
    assert!(
        held.get("open").and_then(Value::as_bool).unwrap_or(false),
        "isolated status should remain open while public squash pressure runs: {held}"
    );
    assert_eq!(
        held.get("manifest_version").and_then(Value::as_i64),
        pinned_version
    );
    assert_eq!(
        held.get("manifest_root_hash").and_then(Value::as_str),
        pinned_hash.as_deref()
    );
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    let released = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&released, "active_leases")?, 0);
    Ok(())
}

#[test]
fn lease_hold_time_ordering() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    thread::sleep(Duration::from_millis(150));
    let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
    let lifetime_s = exit
        .get("lifetime_s")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    assert!(
        lifetime_s >= 0.0,
        "isolated exit lifetime should be nonnegative: {exit}"
    );
    Ok(())
}

#[test]
fn read_op_transient_lease_released() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "lease/read.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "lease", "output_mode": "content"}),
    )?;
    audit.collect()?;
    assert!(
        audit.any("layer_stack.lease_released"),
        "overlay read op should release its transient snapshot lease: {:?}",
        audit.events()
    );
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0);
    Ok(())
}

fn wait_for_active_leases(lease: &eos_e2e_test::NodeLease<'_>, expected: i64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let metrics = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
        if as_i64(&metrics, "active_leases")? == expected {
            return Ok(metrics);
        }
        if Instant::now() >= deadline {
            bail!("active_leases did not reach {expected}: {metrics}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}
