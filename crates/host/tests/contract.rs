//! Host-side conformance (SPEC §9.3): `host` encodes requests
//! that reproduce the frozen request fixtures byte-for-byte, with NO compiled
//! code shared with the box side — this is the drift defense for the
//! deliberately duplicated host wire vocabulary.
#![cfg(feature = "e2e-support")]

use serde_json::json;

use host::e2e_support::{
    encode_request_with_metadata, CONNECT_RETRY_DELAYS_S, DAEMON_AUTH_FIELD, DAEMON_PROTOCOL_FIELD,
    DAEMON_PROTOCOL_VERSION, MAX_REQUEST_BYTES,
};

const READ_FILE_FIXTURE: &[u8] =
    include_bytes!("../../shared/protocol/fixtures/wire_messages/read_file_request.json");
const HEARTBEAT_FIXTURE: &[u8] =
    include_bytes!("../../shared/protocol/fixtures/wire_messages/heartbeat_request.json");
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
    let encoded = encode_request_with_metadata("sandbox.file.read", invocation_id, &args, None);
    assert_eq!(
        encoded,
        fixture_line(READ_FILE_FIXTURE),
        "host-encoded read_file request must be byte-identical to the fixture"
    );
}

/// The heartbeat fixture: protocol version stamped by the host
/// lands in caller insertion order, invocation id appended.
#[test]
fn stamped_encoder_reproduces_heartbeat_fixture() {
    let invocation_id = "00000000000000000000000000000001";
    let args = json!({
        "layer_stack_root": "/eos/layer-stack",
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        "invocation_ids": [invocation_id],
    });
    let encoded =
        encode_request_with_metadata("sandbox.call.heartbeat", invocation_id, &args, None);
    assert_eq!(
        encoded,
        fixture_line(HEARTBEAT_FIXTURE),
        "host-encoded heartbeat request must be byte-identical to the fixture"
    );
}

/// The auth token is a TOP-LEVEL request field, never inside args.
#[test]
fn auth_token_is_stamped_top_level() {
    let encoded =
        encode_request_with_metadata("sandbox.call.heartbeat", "i1", &json!({}), Some("tok-1"));
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("decode");
    assert_eq!(value[DAEMON_AUTH_FIELD], json!("tok-1"));
    assert!(value["args"].get(DAEMON_AUTH_FIELD).is_none());
}

/// Reserved wire-version metadata is owned by the host encoder.
#[test]
fn stamped_encoder_overwrites_caller_protocol_version() {
    let encoded = encode_request_with_metadata(
        "sandbox.call.heartbeat",
        "i1",
        &json!({ DAEMON_PROTOCOL_FIELD: 999 }),
        None,
    );
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("decode");
    assert_eq!(
        value["args"][DAEMON_PROTOCOL_FIELD],
        json!(DAEMON_PROTOCOL_VERSION)
    );
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
