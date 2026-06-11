//! Adapter-side isolated-workspace op tests. Lifecycle behavior lives in
//! `eos-workspace-runtime/tests/`; dispatch-level coverage lives in the
//! daemon's `phase2_read_paths` integration tests.

use super::*;

#[test]
fn host_ram_pressure_error_keeps_capacity_details() {
    let response = error_payload(&IsolatedError::HostRamPressure {
        required_bytes: 30,
        budget_bytes: 29,
    });
    assert_eq!(response["success"], false);
    assert_eq!(response["error"]["kind"], "host_ram_pressure");
    assert_eq!(response["error"]["details"]["required_bytes"], 30);
    assert_eq!(response["error"]["details"]["budget_bytes"], 29);
}
