//! Envelope / error-surface contract tests (plan §6 foundational tier).
//!
//! Asserts the daemon's wire-error catalog directly over `eos-protocol`:
//! unknown op, malformed frame, oversized request, and TCP auth. All four are
//! observed as structured error envelopes (`success:false` + `error.kind`).

use std::sync::Arc;

use anyhow::{Context, Result};
use eos_e2e_test::client::error_kind;
use eos_e2e_test::{live_pool, NodePool};
use eos_protocol::{ops, MAX_REQUEST_BYTES};
use serde_json::json;

fn live_pool_or_skip() -> Result<Option<Arc<NodePool>>> {
    let Some(pool) = live_pool()? else {
        eprintln!("skipping live eos-e2e-test; enable with `--features e2e`");
        return Ok(None);
    };
    Ok(Some(pool))
}

#[test]
fn unknown_op_rejected() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let resp = lease.call("api.totally.bogus.op", json!({}))?;
    assert_eq!(
        error_kind(&resp),
        Some("unknown_op"),
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
        error_kind(&resp),
        Some("bad_json"),
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
        ops::API_V1_WRITE_FILE,
        json!({"path": "big.txt", "content": huge, "overwrite": true}),
    )?;
    assert_eq!(
        error_kind(&resp),
        Some("request_too_large"),
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
        .request(ops::API_V1_HEARTBEAT, "contract-bad-auth", &json!({}))
        .context("heartbeat with wrong token")?;
    assert_eq!(
        error_kind(&resp),
        Some("unauthorized"),
        "a wrong auth token must surface Unauthorized: {resp}"
    );

    let none = lease.client().with_token(None);
    let resp = none
        .request(ops::API_V1_HEARTBEAT, "contract-no-auth", &json!({}))
        .context("heartbeat with no token")?;
    assert_eq!(
        error_kind(&resp),
        Some("unauthorized"),
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

    let entered = lease.call(ops::API_ISOLATED_WORKSPACE_ENTER, json!({}))?;
    assert!(
        eos_e2e_test::client::is_success(&entered),
        "isolated enter must succeed before checking plugin isolation gate: {entered}"
    );

    let blocked = lease.call("plugin.lsp.not_loaded_yet", json!({}))?;
    let _ = lease.call(ops::API_ISOLATED_WORKSPACE_EXIT, json!({}));
    assert_eq!(
        error_kind(&blocked),
        Some("forbidden_in_isolated_workspace"),
        "plugin-family ops must be blocked while isolated mode is active: {blocked}"
    );
    Ok(())
}
