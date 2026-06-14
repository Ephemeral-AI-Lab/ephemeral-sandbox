//! Daemon conformance (SPEC §9.2): request fixtures decode byte-stably and
//! response fixtures decode canonical-equal. Fixtures are immutable ground truth from the live runtime
//! (`json.dumps(separators=(",",":")) + "\n"`).

use daemon::wire::message::{decode, encode, WireMessage};
use serde_json::{json, Value};

mod support;

use support::canonical::canonicalize;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../shared/protocol/fixtures/wire_messages/",
            $name
        ))
    };
}

/// Requests are byte-identity: decode -> encode == original.
#[test]
fn requests_byte_stable() -> TestResult {
    let raws: &[&[u8]] = &[
        fixture!("read_file_request.json"),
        fixture!("heartbeat_request.json"),
    ];
    for raw in raws {
        let env = decode(raw)?;
        match &env {
            WireMessage::Request(_) => {}
            WireMessage::Response(_) => {
                return Err(std::io::Error::other(format!(
                    "expected request, got response: {env:?}"
                ))
                .into());
            }
        }
        let reencoded = encode(&env)?;
        assert_eq!(
            reencoded,
            raw.to_vec(),
            "byte-stable round-trip failed for fixture: {}",
            String::from_utf8_lossy(raw)
        );
    }
    Ok(())
}

/// Responses are canonical-equal.
#[test]
fn responses_canonical_stable() -> TestResult {
    let raws: &[&[u8]] = &[
        fixture!("read_file_response.json"),
        fixture!("heartbeat_response.json"),
        fixture!("readiness_response.json"),
        fixture!("error_unknown_op.json"),
        fixture!("error_request_too_large.json"),
    ];
    for raw in raws {
        let env = decode(raw)?;
        let value = match &env {
            WireMessage::Response(v) => v.clone(),
            other => {
                return Err(
                    std::io::Error::other(format!("expected response, got {other:?}")).into(),
                );
            }
        };
        // Re-encode then re-decode; the canonical form must be stable.
        let reencoded = encode(&env)?;
        let redecoded = decode(&reencoded)?;
        let value2 = match redecoded {
            WireMessage::Response(v) => v,
            other => {
                return Err(
                    std::io::Error::other(format!("expected response, got {other:?}")).into(),
                );
            }
        };
        assert_eq!(canonicalize(&value), canonicalize(&value2));
    }
    Ok(())
}

#[test]
fn readiness_response_canonicalizes_dynamic_runtime_fields() -> TestResult {
    let base: Value = serde_json::from_slice(fixture!("readiness_response.json"))?;
    let mut varied = base.clone();
    varied["result"]["daemon_pid"] = json!(99_999);
    varied["result"]["uptime_s"] = json!(86_400.125);

    assert_ne!(base["result"]["daemon_pid"], varied["result"]["daemon_pid"]);
    assert_ne!(base["result"]["uptime_s"], varied["result"]["uptime_s"]);
    assert_eq!(canonicalize(&base), canonicalize(&varied));
    Ok(())
}

/// The required protocol-version field lives INSIDE args.
#[test]
fn protocol_version_field_inside_args() -> TestResult {
    let raw = fixture!("read_file_request.json");
    let value: Value = serde_json::from_slice(raw)?;
    assert!(value.get("_eos_daemon_protocol_version").is_none());
    assert_eq!(
        value["args"]["_eos_daemon_protocol_version"],
        Value::Number(1.into())
    );
    Ok(())
}
