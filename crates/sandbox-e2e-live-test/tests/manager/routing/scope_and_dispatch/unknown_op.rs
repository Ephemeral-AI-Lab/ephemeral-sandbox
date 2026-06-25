use crate::support::{self, assertion as assert};

// build.rs slug => `routing_scope_and_dispatch_unknown_op`, mounted by tests/manager.rs.
#[test]
fn unknown_manager_op_is_unknown_op() {
    let Some(h) = support::harness() else {
        return; // skip when not under eos-e2e (EOS_E2E_RUN_ROOT unset)
    };

    // N1 owns no sandbox -> there is no id to key on, so it writes no
    // exchange.jsonl; it asserts purely on the returned CallRecord in-process.
    // The public CLI rejects operations missing from the manager catalog before
    // sending a request to the manager router.
    let rec = h.cli().manager("definitely_not_an_op", &[]);
    assert::err_kind_at(&rec, "invalid_request", 2);
}
