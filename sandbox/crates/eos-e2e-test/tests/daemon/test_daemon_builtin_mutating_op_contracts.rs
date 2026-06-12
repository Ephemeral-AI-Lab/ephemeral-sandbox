//! Live contracts for built-in ops that the registration smoke skips because
//! they mutate daemon state or are intentionally harness-gated.

use anyhow::{Context, Result};
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::support::{
    as_bool, as_str, envelope_error_kind, envelope_error_kind_or_status, envelope_result,
    envelope_status, live_pool_or_skip, reset_isolated_workspaces,
};

#[test]
fn isolated_workspace_lifecycle_ops_are_live() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    reset_isolated_workspaces(&lease);

    let caller_id = lease.caller_id().to_owned();
    let body = (|| -> Result<()> {
        let before = lease.call_ok(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({}))?;
        assert!(
            !open_callers(&before).contains(&caller_id.as_str()),
            "fresh caller should not start open: {before}"
        );

        let enter = lease.call_ok(catalog::SANDBOX_ISOLATION_ENTER, json!({}))?;
        assert!(
            !as_str(&enter, "workspace_handle_id")?.is_empty(),
            "enter must allocate an isolated workspace handle: {enter}"
        );

        let open = lease.call_ok(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({}))?;
        assert!(
            open_callers(&open).contains(&caller_id.as_str()),
            "list_open should expose the entered caller: {open}"
        );

        let status = lease.call_ok(catalog::SANDBOX_ISOLATION_STATUS, json!({}))?;
        assert!(
            as_bool(&status, "open")?,
            "status should report the caller open after enter: {status}"
        );

        let exit = lease.call_ok(catalog::SANDBOX_ISOLATION_EXIT, json!({"grace_s": 0.0}))?;
        let inspection = exit.get("inspection").context("exit inspection")?;
        assert_eq!(
            inspection
                .get("handle_registered_after")
                .and_then(Value::as_bool),
            Some(false),
            "exit should unregister the isolated handle: {exit}"
        );

        let closed = lease.call_ok(catalog::SANDBOX_ISOLATION_STATUS, json!({}))?;
        assert!(
            !as_bool(&closed, "open")?,
            "status should report closed after exit: {closed}"
        );

        let after = lease.call_ok(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({}))?;
        assert!(
            !open_callers(&after).contains(&caller_id.as_str()),
            "list_open should drop the caller after exit: {after}"
        );
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(catalog::SANDBOX_ISOLATION_EXIT, json!({"grace_s": 0.0}));
    }
    body
}

#[test]
fn isolated_workspace_test_reset_op_is_live_and_harness_gated() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let reset = lease.call(catalog::SANDBOX_ISOLATION_TEST_RESET, json!({}))?;
    assert_ne!(
        envelope_error_kind_or_status(&reset)?,
        "unknown_op",
        "test_reset should be a registered built-in op: {reset}"
    );
    if envelope_status(&reset)? == "ok" {
        let reset_result = envelope_result(&reset)?;
        assert!(
            as_bool(reset_result, "reset")?,
            "harness-enabled reset should report reset=true: {reset}"
        );
        return Ok(());
    }
    assert_eq!(
        envelope_error_kind(&reset)?,
        "forbidden",
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
