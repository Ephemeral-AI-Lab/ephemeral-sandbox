//! WireMessage and error-surface contract tests.
//!
//! Asserts the daemon's wire-error catalog directly over the checked wire contract:
//! unknown op, malformed frame, oversized request, and TCP auth. All four are
//! observed as structured `OperationEnvelope` error responses.

use anyhow::{Context, Result};
use eos_operation::core::catalog;
use eos_sandbox_host::MAX_REQUEST_BYTES;
use serde_json::json;

use crate::support::{envelope_error_kind, envelope_status, live_pool_or_skip};

#[test]
fn unknown_op_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let resp = lease.call("api.totally.bogus.op", json!({}))?;
    assert_eq!(
        envelope_error_kind(&resp)?,
        "unknown_op",
        "unknown op must surface UnknownOp: {resp}"
    );
    Ok(())
}

#[test]
fn bad_json_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A malformed frame that is not valid JSON.
    let resp = lease
        .client()
        .request_raw(b"{ this is not valid json \n")
        .context("send malformed frame")?;
    assert_eq!(
        envelope_error_kind(&resp)?,
        "bad_json",
        "malformed frame must surface BadJson: {resp}"
    );
    Ok(())
}

#[test]
fn oversized_request_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A request line strictly larger than MAX_REQUEST_BYTES (16 MiB).
    let huge = "x".repeat(MAX_REQUEST_BYTES + 1024);
    let resp = lease.call(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "big.txt", "content": huge, "overwrite": true}),
    )?;
    assert_eq!(
        envelope_error_kind(&resp)?,
        "request_too_large",
        "an oversized request line must surface RequestTooLarge: {}",
        resp.get("error").map_or(&resp, |e| e)
    );
    Ok(())
}

#[test]
fn unauthorized_tcp_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let wrong = lease
        .client()
        .with_token(Some("definitely-wrong-token".to_owned()));
    let resp = wrong
        .request(
            catalog::SANDBOX_CALL_HEARTBEAT,
            "contract-bad-auth",
            &json!({}),
        )
        .context("heartbeat with wrong token")?;
    assert_eq!(
        envelope_error_kind(&resp)?,
        "unauthorized",
        "a wrong auth token must surface Unauthorized: {resp}"
    );

    let none = lease.client().with_token(None);
    let resp = none
        .request(
            catalog::SANDBOX_CALL_HEARTBEAT,
            "contract-no-auth",
            &json!({}),
        )
        .context("heartbeat with no token")?;
    assert_eq!(
        envelope_error_kind(&resp)?,
        "unauthorized",
        "a missing auth token must surface Unauthorized: {resp}"
    );
    Ok(())
}

#[test]
fn forbidden_in_isolated_workspace_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;

    let entered = lease.call(catalog::SANDBOX_ISOLATION_ENTER, json!({}))?;
    assert!(
        envelope_status(&entered)? == "ok",
        "isolated enter must succeed before checking plugin isolation gate: {entered}"
    );

    let blocked = lease.call("plugin.lsp.not_loaded_yet", json!({}))?;
    let _ = lease.call(catalog::SANDBOX_ISOLATION_EXIT, json!({}));
    assert_eq!(
        envelope_error_kind(&blocked)?,
        "forbidden_in_isolated_workspace",
        "plugin-family ops must be blocked while isolated mode is active: {blocked}"
    );
    Ok(())
}
