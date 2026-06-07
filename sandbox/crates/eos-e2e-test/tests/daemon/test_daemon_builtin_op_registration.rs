//! Every built-in daemon op is actually wire-routed (end-to-end registration).
//!
//! Complements the in-process `registry` unit test by proving the live `eosd`
//! serves each `BUILTIN_DAEMON_OPS` string over TCP: a registered handler returns
//! success OR a non-`unknown_op` error (e.g. a missing-arg `invalid_envelope`),
//! whereas an unregistered string returns `unknown_op`.

use anyhow::Result;
use eos_e2e_test::client::error_kind;
use eos_protocol::ops;
use serde_json::json;

use crate::support::live_pool_or_skip;

/// State-toggling ops are skipped: called with injected args they would mutate
/// the lease (enter isolated mode, reset the audit floor) and perturb the loop.
/// Their dispatch is proven by the dedicated tier tests instead.
const SKIP: &[&str] = &[
    ops::API_ISOLATED_WORKSPACE_ENTER,
    ops::API_ISOLATED_WORKSPACE_EXIT,
    ops::API_ISOLATED_WORKSPACE_TEST_RESET,
    ops::API_AUDIT_RESET_FLOOR,
    // Would cancel + discard every workspace run in the shared lease.
    ops::API_V1_CANCEL_WORKSPACE_RUNS,
];

#[test]
fn every_builtin_op_is_wire_routed() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for op in ops::BUILTIN_DAEMON_OPS {
        if SKIP.contains(op) {
            continue;
        }
        let resp = lease.call(op, json!({}))?;
        assert_ne!(
            error_kind(&resp),
            Some("unknown_op"),
            "builtin op {op} must be registered over the wire: {resp}"
        );
    }
    // Negative control: an unregistered op must surface unknown_op.
    let bogus = lease.call("api.totally.bogus.op", json!({}))?;
    assert_eq!(
        error_kind(&bogus),
        Some("unknown_op"),
        "an unregistered op must surface unknown_op: {bogus}"
    );
    Ok(())
}
