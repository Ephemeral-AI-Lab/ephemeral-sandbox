//! Every built-in daemon op is actually wire-routed (end-to-end registration)
//! under its canonical `sandbox.*` name.
//!
//! Complements the in-process `registry` unit test by proving the live `eosd`
//! serves each catalog spelling over TCP: a registered handler returns success
//! OR a non-`unknown_op` error (e.g. a missing-arg `invalid_request`), whereas
//! an unregistered string returns `unknown_op`.

use anyhow::Result;
use eos_operation::core::catalog::{BuiltinOp, ServedBy, BUILTIN_OPS};
use serde_json::json;

use crate::support::{envelope_error_kind, envelope_error_kind_or_status, live_pool_or_skip};

/// State-toggling ops are skipped: called with injected args they would mutate
/// the lease (enter isolated mode) and perturb the loop. Their dispatch is
/// proven by the dedicated tier tests instead.
const SKIP: &[BuiltinOp] = &[
    BuiltinOp::IsolatedWorkspaceEnter,
    BuiltinOp::IsolatedWorkspaceExit,
    BuiltinOp::IsolatedWorkspaceTestReset,
    // Would cancel + discard every workspace run in the shared lease.
    BuiltinOp::CancelWorkspaceRuns,
];

#[test]
fn every_builtin_op_is_wire_routed_under_its_canonical_name() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    for contract in BUILTIN_OPS {
        if contract.served_by != ServedBy::Daemon || SKIP.contains(&contract.op) {
            continue;
        }
        let resp = lease.call(contract.name, json!({}))?;
        assert_ne!(
            envelope_error_kind_or_status(&resp)?,
            "unknown_op",
            "catalog spelling {} must be registered over the wire: {resp}",
            contract.name
        );
    }
    // Negative control: an unregistered op must surface unknown_op.
    let bogus = lease.call("api.totally.bogus.op", json!({}))?;
    assert_eq!(
        envelope_error_kind(&bogus)?,
        "unknown_op",
        "an unregistered op must surface unknown_op: {bogus}"
    );
    Ok(())
}
