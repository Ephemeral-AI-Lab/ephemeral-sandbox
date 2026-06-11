//! Host-side conformance (SPEC §9.3): `eos-sandbox-host` encodes requests
//! that reproduce the frozen request fixtures byte-for-byte, with NO compiled
//! code shared with the box side — this is the drift defense for the
//! deliberately duplicated wire vocabulary in `eos_sandbox_host::protocol`.

use serde_json::json;

use eos_sandbox_host::protocol::{
    raw_envelope_bytes, stamped_envelope_bytes, CONNECT_RETRY_DELAYS_S, DAEMON_AUTH_FIELD,
    DAEMON_PROTOCOL_FIELD, DAEMON_PROTOCOL_VERSION, MAX_REQUEST_BYTES,
};

const READ_FILE_FIXTURE: &[u8] =
    include_bytes!("../../../contract/fixtures/envelopes/read_file_request.json");
const HEARTBEAT_FIXTURE: &[u8] =
    include_bytes!("../../../contract/fixtures/envelopes/heartbeat_request.json");
const READINESS_FIXTURE: &[u8] =
    include_bytes!("../../../contract/fixtures/envelopes/readiness_request.json");

fn fixture_line(raw: &[u8]) -> Vec<u8> {
    let mut line = raw.to_vec();
    while line.last() == Some(&b'\n') {
        line.pop();
    }
    line
}

/// The host encoder reproduces the read_file request fixture: caller-ordered
/// args with the protocol version explicit mid-object and the invocation id
/// stamp appended last.
#[test]
fn stamped_encoder_reproduces_read_file_fixture() {
    let invocation_id = "00000000000000000000000000000001";
    let args = json!({
        "layer_stack_root": "/eos/layer-stack",
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        "path": "/workspace/repo/README.md",
        "caller_id": "caller-1",
    });
    let encoded = stamped_envelope_bytes("sandbox.file.read", invocation_id, &args, None);
    assert_eq!(
        encoded,
        fixture_line(READ_FILE_FIXTURE),
        "host-encoded read_file request must be byte-identical to the fixture"
    );
}

/// The heartbeat fixture: protocol version stamped by the host (or_insert)
/// lands in caller insertion order, invocation id appended.
#[test]
fn stamped_encoder_reproduces_heartbeat_fixture() {
    let invocation_id = "00000000000000000000000000000001";
    let args = json!({
        "layer_stack_root": "/eos/layer-stack",
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        "invocation_ids": [invocation_id],
    });
    let encoded = stamped_envelope_bytes("sandbox.call.heartbeat", invocation_id, &args, None);
    assert_eq!(
        encoded,
        fixture_line(HEARTBEAT_FIXTURE),
        "host-encoded heartbeat request must be byte-identical to the fixture"
    );
}

/// The readiness fixture pins the UNSTAMPED shape: no protocol version, no
/// args-level invocation id.
#[test]
fn raw_encoder_reproduces_readiness_fixture() {
    let encoded = raw_envelope_bytes(
        "sandbox.runtime.ready",
        "00000000000000000000000000000001",
        &json!({"layer_stack_root": "/eos/layer-stack"}),
        None,
    );
    assert_eq!(
        encoded,
        fixture_line(READINESS_FIXTURE),
        "host-encoded readiness request must be byte-identical to the fixture"
    );
}

/// The auth token is a TOP-LEVEL envelope field, never inside args.
#[test]
fn auth_token_is_stamped_top_level() {
    let encoded = stamped_envelope_bytes("sandbox.call.heartbeat", "i1", &json!({}), Some("tok-1"));
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("decode");
    assert_eq!(value[DAEMON_AUTH_FIELD], json!("tok-1"));
    assert!(value["args"].get(DAEMON_AUTH_FIELD).is_none());
}

/// The duplicated host-side limits match the frozen contract.
#[test]
fn host_wire_constants_match_frozen_contract() {
    assert_eq!(MAX_REQUEST_BYTES, 16 * 1024 * 1024);
    assert_eq!(CONNECT_RETRY_DELAYS_S, [0.25, 0.5, 1.0, 2.0]);
    assert_eq!(DAEMON_PROTOCOL_VERSION, 1);
    assert_eq!(DAEMON_PROTOCOL_FIELD, "_eos_daemon_protocol_version");
    assert_eq!(DAEMON_AUTH_FIELD, "_eos_daemon_auth_token");
}
