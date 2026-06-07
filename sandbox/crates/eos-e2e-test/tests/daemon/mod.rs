//! Daemon control-plane tier: the in-sandbox parent process and its dispatch,
//! invocation registry, and runtime-identity surfaces over the live wire.
//!
//! Complements `core/` (which already covers heartbeat-idle, inflight-idle,
//! envelope errors, and the basic ready handshake) with the registry-coupled
//! behavior that needs a real in-flight invocation: inflight accounting,
//! heartbeat `touched` discrimination, cancel envelopes, end-to-end op
//! registration, and the daemon-identity / dispatch-timing fields.

#![allow(dead_code)]

#[path = "../support/mod.rs"]
mod support;

use std::thread::{self, JoinHandle};

use anyhow::Result;
use eos_e2e_test::NodeLease;
use eos_protocol::ops;
use serde_json::{json, Value};

const E2E_CONFIG: &str = "crates/eos-e2e-test/tests/daemon/config/default.test.yml";

mod test_daemon_audit_pagination_and_reset;
mod test_daemon_builtin_mutating_op_contracts;
mod test_daemon_builtin_op_registration;
mod test_daemon_cancel_control;
mod test_daemon_heartbeat_control;
mod test_daemon_inflight_control;
mod test_daemon_inflight_ttl_reaper;
mod test_daemon_plugin_background_control;
mod test_daemon_runtime_identity;

/// Fire a backgrounded `exec_command` on its own thread under an explicit
/// `invocation_id`, keeping the invocation registered in-flight for the yield
/// window (~5s) so the control-plane ops can observe it. The raw client does NOT
/// inject `caller_id`/`layer_stack_root`, so they are passed explicitly to match
/// the lease the polling side queries; `background: true` is required for the
/// `count_by_caller` inflight filter. `sleep 8` outlives the yield then
/// self-exits (no orphan). Callers MUST join the handle before the lease drops.
fn spawn_inflight_exec(lease: &NodeLease<'_>, invocation_id: &str) -> JoinHandle<Result<Value>> {
    let client = lease.client().clone();
    let root = lease.root().to_owned();
    let caller_id = lease.caller_id().to_owned();
    let invocation_id = invocation_id.to_owned();
    thread::spawn(move || {
        client.request(
            ops::API_V1_EXEC_COMMAND,
            &invocation_id,
            &json!({
                "layer_stack_root": root,
                "caller_id": caller_id,
                "background": true,
                "cmd": "sleep 8",
                "yield_time_ms": 5000,
                "timeout_seconds": 120,
                "max_output_tokens": 200
            }),
        )
    })
}
