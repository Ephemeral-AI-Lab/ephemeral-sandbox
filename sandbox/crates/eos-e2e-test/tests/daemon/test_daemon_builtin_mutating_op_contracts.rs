//! Live contracts for built-in ops that the registration smoke skips because
//! they mutate daemon state or are intentionally harness-gated.

use anyhow::{Context, Result};
use eos_e2e_test::client::error_kind;
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{as_bool, as_str, live_pool_or_skip, reset_isolated_workspaces};

#[test]
fn audit_reset_floor_is_live_and_config_gated() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let reset = lease.call_ok(ops::API_AUDIT_RESET_FLOOR, json!({}))?;
    assert!(
        as_bool(&reset, "reset")?,
        "daemon test config should enable audit floor reset: {reset}"
    );
    Ok(())
}

#[test]
fn isolated_workspace_lifecycle_ops_are_live() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);

    let caller_id = lease.caller_id().to_owned();
    let body = (|| -> Result<()> {
        let before = lease.call_ok(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({}))?;
        assert!(
            !open_callers(&before).contains(&caller_id.as_str()),
            "fresh caller should not start open: {before}"
        );

        let enter = lease.call_ok(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
        assert!(
            !as_str(&enter, "workspace_handle_id")?.is_empty(),
            "enter must allocate an isolated workspace handle: {enter}"
        );

        let open = lease.call_ok(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({}))?;
        assert!(
            open_callers(&open).contains(&caller_id.as_str()),
            "list_open should expose the entered caller: {open}"
        );

        let status = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
        assert!(
            as_bool(&status, "open")?,
            "status should report the caller open after enter: {status}"
        );

        let exit = lease.call_ok(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.0}))?;
        let inspection = exit.get("inspection").context("exit inspection")?;
        assert_eq!(
            inspection
                .get("handle_registered_after")
                .and_then(Value::as_bool),
            Some(false),
            "exit should unregister the isolated handle: {exit}"
        );

        let closed = lease.call_ok(ops::API_ISOLATED_WORKSPACE_STATUS, json!({}))?;
        assert!(
            !as_bool(&closed, "open")?,
            "status should report closed after exit: {closed}"
        );

        let after = lease.call_ok(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({}))?;
        assert!(
            !open_callers(&after).contains(&caller_id.as_str()),
            "list_open should drop the caller after exit: {after}"
        );
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(ops::API_ISOLATED_WORKSPACE_EXIT, json!({"grace_s": 0.0}));
    }
    body
}

#[test]
fn isolated_workspace_test_reset_op_is_live_and_harness_gated() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let reset = lease.call(ops::API_ISOLATED_WORKSPACE_TEST_RESET, json!({}))?;
    assert_ne!(
        error_kind(&reset),
        Some("unknown_op"),
        "test_reset should be a registered built-in op: {reset}"
    );
    if reset.get("success").and_then(Value::as_bool) == Some(true) {
        assert!(
            as_bool(&reset, "reset")?,
            "harness-enabled reset should report reset=true: {reset}"
        );
        return Ok(());
    }
    assert_eq!(
        error_kind(&reset),
        Some("forbidden"),
        "without the isolated test harness env, reset should fail with the stable gate: {reset}"
    );
    let message = reset
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true"),
        "forbidden response should name the required harness env: {reset}"
    );
    Ok(())
}

fn open_callers(response: &Value) -> Vec<&str> {
    response
        .get("open_caller_ids")
        .and_then(Value::as_array)
        .map(|callers| callers.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
