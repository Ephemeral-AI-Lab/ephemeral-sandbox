use super::*;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn error_kind_snake_case_wire() -> TestResult {
    let v = serde_json::to_value(ErrorKind::ForbiddenInIsolatedWorkspace)?;
    assert_eq!(
        v,
        Value::String("forbidden_in_isolated_workspace".to_owned())
    );
    assert_eq!(
        serde_json::to_value(ErrorKind::UnknownOp)?,
        Value::String("unknown_op".to_owned())
    );
    Ok(())
}

#[test]
fn encode_appends_single_newline() -> TestResult {
    let env = WireMessage::Response(serde_json::json!({"success": true, "touched": 0}));
    let bytes = encode(&env)?;
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_ne!(bytes[bytes.len() - 2], b'\n');
    Ok(())
}

#[test]
fn request_args_order_preserved_roundtrip() -> TestResult {
    let raw = b"{\"op\":\"x\",\"invocation_id\":\"i\",\"args\":{\"z\":1,\"a\":2,\"_eos_daemon_protocol_version\":1}}\n";
    let env = decode(raw)?;
    assert!(matches!(env, WireMessage::Request(_)));
    assert_eq!(encode(&env)?, raw);
    Ok(())
}

// Build arbitrary JSON values with only finite numbers (NaN/Inf are not JSON).
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        ".*".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(("[a-z]{1,6}", inner), 0..6)
                .prop_map(|kvs| { Value::Object(kvs.into_iter().collect()) }),
        ]
    })
}

proptest! {
    #[test]
    fn decode_encode_roundtrips_requests(op in "[a-z.]{1,12}", id in "[a-z0-9]{0,16}", args in arb_json()) {
        let args = if args.is_object() { args } else { serde_json::json!({"v": args}) };
        let env = WireMessage::Request(Request { op, invocation_id: id, args });
        let bytes = encode(&env)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let back = decode(&bytes)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(env, back);
    }
}
