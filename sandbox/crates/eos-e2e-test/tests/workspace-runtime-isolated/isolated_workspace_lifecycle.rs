//! Isolated-workspace lifecycle tests.
//!
//! Exercises the real SetNs / ns-holder / veth / cgroup machinery via
//! `enter` → (isolated write/read) → `status` → `exit`, asserting on the op
//! responses: the manifest pin on enter/status, isolated `mutation_source`,
//! discard-on-exit (the write is never OCC-published), and the exit `inspection`
//! teardown facts.

use anyhow::{Context, Result};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    as_bool, as_i64, as_str, live_pool_or_skip, wait_for_command_stdout_contains,
    wait_for_session_count,
};

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
fn enter_rejects_active_command_session_and_repeated_enter_reports_already_open() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "bash -lc 'printf ACTIVE; sleep 30'",
            "yield_time_ms": 500,
            "timeout_seconds": 60,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    let session_id = as_str(&exec, "command_session_id")?.to_owned();
    // `printf ACTIVE` may not reach the transcript within the 500ms yield under
    // emulation; poll until it does (still proves the session is actively live).
    wait_for_command_stdout_contains(&lease, &session_id, "ACTIVE")?;

    let body = (|| -> Result<()> {
        let rejected = lease.call(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
        assert_eq!(
            rejected.get("success").and_then(Value::as_bool),
            Some(false),
            "enter must reject instead of silently cleaning up an active command session: {rejected}"
        );
        assert_eq!(
            rejected
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str),
            Some("active_background_work"),
            "active command session rejection should use a stable error kind: {rejected}"
        );
        assert_eq!(
            rejected
                .get("error")
                .and_then(|error| error.get("details"))
                .and_then(|details| details.get("active_command_sessions"))
                .and_then(Value::as_i64),
            Some(1),
            "rejection should report active session count: {rejected}"
        );

        lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id}),
        )?;
        wait_for_session_count(&lease, 0)?;

        let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
        assert!(
            !as_str(&enter, "workspace_handle_id")?.is_empty(),
            "{enter}"
        );
        let repeated = lease.call(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
        assert_eq!(
            repeated.get("success").and_then(Value::as_bool),
            Some(false),
            "repeated enter must reject while the handle is open: {repeated}"
        );
        assert_eq!(
            repeated
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str),
            Some("already_open"),
            "repeated enter should report already_open: {repeated}"
        );
        lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}))?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id}),
        );
        let _ = lease.call(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.0}));
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

#[test]
fn isolated_workspace_jsonl_records_lifecycle_and_tool_call_fields() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let caller_id = format!("iws-audit-{}", eos_e2e_test::unique_suffix());
    let path = format!("audit/{}.txt", eos_e2e_test::unique_suffix());

    let enter = lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_ENTER,
        json!({"caller_id": caller_id}),
    )?;
    let handle_id = as_str(&enter, "workspace_handle_id")?.to_owned();
    let write = lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"caller_id": caller_id, "path": path, "content": "isolated audit\n", "overwrite": true}),
    )?;
    assert_eq!(as_str(&write, "workspace")?, "isolated", "{write}");
    let exit = lease.call_ok(
        ops::API_ISOLATED_WORKSPACE_EXIT,
        json!({"caller_id": caller_id}),
    )?;
    assert!(
        as_i64(&exit, "evicted_upperdir_bytes")? > 0,
        "exit should report discarded private bytes: {exit}"
    );

    // Read the audit log from the container host view, not via an ephemeral
    // exec_command: that exec mount-masks /eos to an empty tmpfs, so the
    // daemon-written audit.jsonl is invisible inside it.
    let audit_raw = lease
        .container()
        .exec(&["cat", "/eos/scratch/isolated/audit.jsonl"])
        .context("read isolated audit jsonl from container")?;
    let events = audit_raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let for_handle = |event: &&Value| {
        event
            .get("payload")
            .and_then(|payload| payload.get("workspace_handle_id"))
            .and_then(Value::as_str)
            == Some(handle_id.as_str())
    };
    let enter_event = events
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("sandbox_isolated_workspace_enter")
                && for_handle(event)
        })
        .with_context(|| {
            format!("missing isolated enter audit for handle {handle_id}: {events:?}")
        })?;
    let enter_payload = enter_event.get("payload").context("enter payload")?;
    assert!(
        enter_payload.get("ns_ip").and_then(Value::as_str).is_some(),
        "enter audit should include namespace address: {enter_event}"
    );
    assert!(
        enter_payload
            .get("phases_ms")
            .and_then(Value::as_object)
            .is_some(),
        "enter audit should include phase timings: {enter_event}"
    );

    let tool_event = events
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str)
                == Some("sandbox_isolated_workspace_tool_call")
                && for_handle(event)
        })
        .with_context(|| {
            format!("missing isolated tool-call audit for handle {handle_id}: {events:?}")
        })?;
    let tool_payload = tool_event.get("payload").context("tool-call payload")?;
    assert_eq!(
        tool_payload.get("published").and_then(Value::as_bool),
        Some(false),
        "isolated tool-call audit must record no-publish behavior: {tool_event}"
    );
    assert!(
        tool_payload
            .get("changed_paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| paths
                .iter()
                .any(|value| value.as_str() == Some(path.as_str()))),
        "tool-call audit should include changed path: {tool_event}"
    );

    let exit_event = events
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("sandbox_isolated_workspace_exit")
                && for_handle(event)
        })
        .with_context(|| {
            format!("missing isolated exit audit for handle {handle_id}: {events:?}")
        })?;
    let exit_payload = exit_event.get("payload").context("exit payload")?;
    assert!(
        exit_payload
            .get("upperdir_bytes_discarded")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            > 0,
        "exit audit should report discarded bytes: {exit_event}"
    );
    assert_eq!(
        exit_payload.get("scratch_removed").and_then(Value::as_bool),
        Some(true),
        "exit audit should report scratch cleanup: {exit_event}"
    );
    assert!(
        exit_payload
            .get("inspection")
            .and_then(Value::as_object)
            .is_some(),
        "exit audit should include teardown inspection: {exit_event}"
    );
    Ok(())
}
