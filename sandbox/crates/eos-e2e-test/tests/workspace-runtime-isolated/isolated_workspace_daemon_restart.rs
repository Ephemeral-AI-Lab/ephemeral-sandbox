use anyhow::Result;
use eos_daemon::wire::ops;
use serde_json::{json, Value};

use crate::support::{
    as_bool, as_i64, live_pool_or_skip, reset_isolated_workspaces, wait_for_active_leases,
};

/// A daemon restart runs `reap_persisted_orphans`
/// (`sandbox/crates/eos-isolated-workspace/src/sessions/gc.rs`): it clears
/// in-memory handles, releases each persisted snapshot lease, kills holders, and
/// removes veth/cgroup/scratch before serving enters. No e2e exercised that
/// path. Here an isolated handle is opened (holding a snapshot lease), the daemon
/// is hard-restarted, and startup reconciliation must reap the now-orphaned
/// handle: `list_open` empties, no snapshot lease is stranded, and the daemon
/// serves fresh enters again.
#[test]
fn daemon_restart_reaps_orphaned_isolated_handle() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);

    let caller = format!("restart-iws-{}", eos_e2e_test::unique_suffix());
    let enter = lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller}),
    )?;
    assert!(
        as_bool(&enter, "success")?,
        "isolated enter should open: {enter}"
    );
    // The open handle holds a snapshot lease on this checkout's LayerStack root.
    let held = wait_for_active_leases(&lease, 1)?;
    assert_eq!(
        as_i64(&held, "active_leases")?,
        1,
        "isolated enter should hold one snapshot lease: {held}"
    );
    let before = lease.call_ok(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({}))?;
    assert!(
        open_contains(&before, &caller),
        "the handle should be open before restart: {before}"
    );

    // Hard-restart the daemon: in-memory state is lost, so the open handle
    // becomes an orphan that startup reconciliation must reap.
    lease.restart_daemon()?;

    let ready = lease.call_ok(ops::API_RUNTIME_READY, json!({}))?;
    assert!(
        as_bool(&ready, "ready")?,
        "daemon must be ready again after restart: {ready}"
    );
    let after = lease.call_ok(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({}))?;
    assert!(
        !open_contains(&after, &caller),
        "startup reconciliation must reap the orphaned isolated handle: {after}"
    );
    let released = wait_for_active_leases(&lease, 0)?;
    assert_eq!(
        as_i64(&released, "active_leases")?,
        0,
        "reconciliation must leave no stranded snapshot lease: {released}"
    );

    // The recovered daemon serves fresh isolated enters again.
    let fresh_caller = format!("restart-iws-fresh-{}", eos_e2e_test::unique_suffix());
    let fresh = lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": fresh_caller}),
    )?;
    assert!(
        as_bool(&fresh, "success")?,
        "a fresh isolated enter should succeed after restart: {fresh}"
    );
    lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": fresh_caller, "grace_s": 0.0}),
    )?;
    let drained = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&drained, "active_leases")?, 0, "{drained}");
    Ok(())
}

fn open_contains(listing: &Value, caller: &str) -> bool {
    listing
        .get("open_caller_ids")
        .and_then(Value::as_array)
        .is_some_and(|open| open.iter().any(|value| value.as_str() == Some(caller)))
}
