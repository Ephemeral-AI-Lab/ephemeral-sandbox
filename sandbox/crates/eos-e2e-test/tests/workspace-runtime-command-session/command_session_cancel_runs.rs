//! Workspace-run cancel surface (§7): the per-caller and whole-sandbox cancel
//! ops tear down command sessions (cancel → discard, never publish), keyed by
//! `caller_id == agent_run_id`.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use eos_e2e_test::{unique_suffix, NodeLease};
use eos_protocol::ops;
use serde_json::json;

use crate::support::{
    array, as_bool, as_i64, as_str, live_pool_or_skip, stdout, wait_for_active_leases,
    wait_for_session_count,
};

/// Start a `sleep 60` session for `caller_id` (or the lease default when `None`).
fn start_sleeping(lease: &NodeLease<'_>, caller_id: Option<&str>, marker: &str) -> Result<String> {
    let mut args = json!({
        "cmd": format!("sh -c 'echo {marker}; sleep 60'"),
        "yield_time_ms": 500,
        "timeout_seconds": 120,
    });
    if let Some(caller_id) = caller_id {
        args["caller_id"] = json!(caller_id);
    }
    let started = lease.call_ok(ops::API_V1_EXEC_COMMAND, args)?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    Ok(as_str(&started, "command_session_id")?.to_owned())
}

/// Live command-session count for one caller (empty `caller_id` counts all).
fn count_for(lease: &NodeLease<'_>, caller_id: &str) -> Result<i64> {
    let count = lease.call_ok(
        ops::API_V1_COMMAND_SESSION_COUNT,
        json!({"caller_id": caller_id}),
    )?;
    as_i64(&count, "count")
}

/// Poll a session's transcript until `marker` appears, confirming the command's
/// write reached the overlay before we cancel it.
fn wait_for_progress(lease: &NodeLease<'_>, session_id: &str, marker: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = lease.call_ok(
            ops::API_V1_COMMAND_READ_PROGRESS,
            json!({"command_session_id": session_id, "last_n_lines": 10}),
        )?;
        if stdout(&progress).contains(marker) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("session {session_id} never produced {marker:?}: {progress}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cancel_workspace_runs_by_caller_id_discards_owner_and_spares_sibling() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let owner = lease.caller_id().to_owned();
    let sibling = format!("{owner}-sibling");

    // Two ephemeral runs for the owner caller, one for a sibling caller.
    let a = start_sleeping(&lease, None, "cancel-owner-a")?;
    let b = start_sleeping(&lease, None, "cancel-owner-b")?;
    let _s = start_sleeping(&lease, Some(&sibling), "cancel-sibling")?;
    assert_eq!(
        count_for(&lease, &owner)?,
        2,
        "owner owns two ephemeral runs"
    );
    assert_eq!(count_for(&lease, &sibling)?, 1, "sibling owns one run");

    let cancelled = lease.call_ok(
        ops::API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER,
        json!({"caller_id": owner}),
    )?;
    assert_eq!(
        as_i64(&cancelled, "cancelled_command_sessions")?,
        2,
        "per-caller cancel tears down exactly the owner's two runs: {cancelled}"
    );
    assert_eq!(
        cancelled["isolated_exited"],
        json!(false),
        "an ephemeral caller has no isolated workspace to exit: {cancelled}"
    );

    // The owner's runs are gone (lease caller == owner); the sibling is spared.
    wait_for_session_count(&lease, 0)?;
    assert_eq!(
        count_for(&lease, &sibling)?,
        1,
        "cancelling one caller must not touch a sibling caller's run"
    );

    // Cancel discards — no completion is parked for the torn-down sessions.
    let drained = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({"command_session_ids": [a, b]}),
    )?;
    assert!(
        array(&drained, "completions")?.is_empty(),
        "a cancelled session must not park a completion: {drained}"
    );

    // Tear the sibling down too and confirm every overlay lease released.
    let _ = lease.call(
        ops::API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER,
        json!({"caller_id": sibling}),
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn cancel_workspace_runs_sweeps_every_caller() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let other = format!("{}-other", lease.caller_id());

    start_sleeping(&lease, None, "sweep-a")?;
    start_sleeping(&lease, Some(&other), "sweep-b")?;
    assert_eq!(
        count_for(&lease, "")?,
        2,
        "two runs across two callers are live"
    );

    let swept = lease.call_ok(ops::API_V1_CANCEL_WORKSPACE_RUNS, json!({}))?;
    assert_eq!(
        as_i64(&swept, "cancelled_command_sessions")?,
        2,
        "the whole-sandbox sweep tears down every caller's runs: {swept}"
    );

    assert_eq!(
        count_for(&lease, "")?,
        0,
        "no command session survives the sweep"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn cancel_workspace_runs_by_caller_id_discards_overlay_writes() -> Result<()> {
    // The load-bearing migration invariant: a cancelled command DISCARDS its
    // overlay and never OCC-merges into the shared LayerStack.
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let owner = lease.caller_id().to_owned();

    // Baseline the shared-LayerStack manifest version.
    let before = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    let v0 = as_i64(&before, "manifest_version")?;

    // A command that writes a workspace file, then blocks. The write lands in the
    // ephemeral overlay's upperdir but is not yet published.
    let marker = format!("cancel-marker-{}.txt", unique_suffix().replace('-', "_"));
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": format!("sh -c 'printf overlay-data > {marker}; echo wrote; sleep 60'"),
            "yield_time_ms": 1000,
            "timeout_seconds": 120,
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    let session_id = as_str(&started, "command_session_id")?.to_owned();
    wait_for_progress(&lease, &session_id, "wrote")?;

    // Cancel the caller's run mid-write via the per-caller op.
    let cancelled = lease.call_ok(
        ops::API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER,
        json!({"caller_id": owner}),
    )?;
    assert_eq!(
        as_i64(&cancelled, "cancelled_command_sessions")?,
        1,
        "{cancelled}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;

    // The shared LayerStack manifest is unchanged — the cancelled write never merged.
    let after = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        as_i64(&after, "manifest_version")?,
        v0,
        "a cancelled command must not OCC-merge its overlay writes: {after}"
    );
    // And the write is absent from the published workspace.
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": marker}))?;
    assert!(
        !as_bool(&read, "exists")?,
        "cancelled overlay write must not be published to the shared workspace: {read}"
    );
    Ok(())
}

/// §10 F3 regression: a backgrounded command that hits its timeout is killed by
/// the reaper SWEEP (no foreground poller), which must PARK a collectable
/// completion. Before the fix the sweep treated the deadline kill as a cancel and
/// pushed nothing, so a fire-and-forget timed-out session was dropped silently and
/// its agent-core background session stayed Running forever. The load-bearing
/// assertion is that a completion is parked and drains at all; the status set
/// tolerates the runner-vs-daemon timeout race.
#[test]
fn background_timeout_parks_collectable_completion() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Background a never-finishing command with a short timeout, then DON'T poll
    // it — only the periodic reaper sweep can reap and park its completion.
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "sh -c 'echo running; sleep 60'",
            "yield_time_ms": 200,
            "timeout_seconds": 1,
        }),
    )?;
    assert_eq!(as_str(&started, "status")?, "running", "{started}");
    let id = as_str(&started, "command_session_id")?.to_owned();

    let deadline = Instant::now() + Duration::from_secs(15);
    let completion = loop {
        let collected = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [&id]}),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            break completion.clone();
        }
        if Instant::now() >= deadline {
            bail!("timed-out background session never parked a completion (F3 regression): {id}");
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let result = &completion["result"];
    assert!(
        matches!(
            as_str(result, "status")?,
            "timed_out" | "error" | "cancelled"
        ),
        "deadline kill should surface as a terminal timeout status: {completion}"
    );
    // A re-collect must not redeliver the drained completion.
    let redelivered = lease.call_ok(
        ops::API_V1_COMMAND_COLLECT_COMPLETED,
        json!({"command_session_ids": [&id]}),
    )?;
    assert!(
        array(&redelivered, "completions")?.is_empty(),
        "collect_completed must remove the delivered timeout completion: {redelivered}"
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}
