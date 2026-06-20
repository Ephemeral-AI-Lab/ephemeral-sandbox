use sandbox_protocol::{decode_request_object, ArgsPresence};
use serde_json::json;

#[test]
fn decode_request_requires_object_args_when_present() {
    let value = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "args": "bad",
    });
    let object = value.as_object().expect("object").clone();
    let err = decode_request_object(object, ArgsPresence::Required)
        .expect_err("non-object args rejected");
    assert_eq!(err.kind(), "invalid_request");
    assert_eq!(err.message(), "args must be an object");
}
