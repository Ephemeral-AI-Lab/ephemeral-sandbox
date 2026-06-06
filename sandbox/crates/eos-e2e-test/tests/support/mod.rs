#![allow(dead_code)]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::{live_pool_with_config, NodeLease, NodePool};
use eos_protocol::ops;
use serde_json::{json, Value};

pub(crate) fn live_pool_or_skip() -> Result<Option<Arc<NodePool>>> {
    let Some(pool) = live_pool_with_config(crate::E2E_CONFIG)? else {
        eprintln!("skipping live eos-e2e-test; enable with `--features e2e`");
        return Ok(None);
    };
    Ok(Some(pool))
}

/// Poll `api.layer_metrics` until `active_leases` settles at `expected`,
/// returning the metrics payload. Layer-lease accounting is asynchronous on the
/// release path, so callers must poll rather than read it instantaneously.
///
/// # Errors
/// Returns an error if the metrics op fails or `active_leases` never reaches
/// `expected` within the deadline.
pub(crate) fn wait_for_active_leases(lease: &NodeLease<'_>, expected: i64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
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

/// Exit every open isolated workspace on this lease's daemon. Used at the start
/// of tests that enter isolated sessions so residue from a prior checkout on a
/// recycled container (e.g. a session leaked when an assertion panicked past its
/// cleanup) does not push past the global isolated-workspace cap. Drains via the
/// ungated `list_open` + `exit` ops (the `test_reset` hook needs a daemon env
/// flag the harness does not set). Best-effort: errors are ignored.
pub(crate) fn reset_isolated_workspaces(lease: &NodeLease<'_>) {
    let Ok(listing) = lease.call(ops::API_ISOLATED_WORKSPACE_LIST_OPEN, json!({})) else {
        return;
    };
    let callers: Vec<String> = listing
        .get("open_caller_ids")
        .and_then(Value::as_array)
        .map(|callers| {
            callers
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for caller_id in callers {
        let _ = lease.call(
            ops::API_ISOLATED_WORKSPACE_EXIT,
            json!({"caller_id": caller_id, "grace_s": 0.0}),
        );
    }
}

/// Poll `api.v1.command_session_count` until `count` settles at `expected`.
///
/// # Errors
/// Returns an error if the count op fails or never reaches `expected` within
/// the deadline.
pub(crate) fn wait_for_session_count(lease: &NodeLease<'_>, expected: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = lease.call_ok(ops::API_V1_COMMAND_SESSION_COUNT, json!({}))?;
        if as_i64(&count, "count")? == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("command_session_count did not reach {expected}: {count}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn as_bool(value: &Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} missing or not bool in {value}"))
}

pub(crate) fn as_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("{key} missing or not i64 in {value}"))
}

pub(crate) fn as_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} missing or not string in {value}"))
}

pub(crate) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("{key} missing or not array in {value}"))
}

pub(crate) fn stdout(value: &Value) -> &str {
    value
        .get("output")
        .and_then(|output| output.get("stdout"))
        .and_then(Value::as_str)
        .or_else(|| value.get("stdout").and_then(Value::as_str))
        .unwrap_or_default()
}

pub(crate) fn conflict_reason(value: &Value) -> String {
    value
        .get("conflict")
        .and_then(|conflict| conflict.get("reason"))
        .and_then(Value::as_str)
        .or_else(|| value.get("conflict_reason").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn conflict_message(value: &Value) -> String {
    value
        .get("conflict")
        .and_then(|conflict| conflict.get("message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("conflict_reason").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}
