use serde_json::json;

#[test]
fn blocking_admission_rejects_immediately_and_recovers_after_release() {
    let admission = BlockingAdmission::new(2);
    let first = admission.try_acquire().expect("first permit");
    let _second = admission.try_acquire().expect("second permit");

    assert_eq!(admission.try_acquire().unwrap_err(), AdmissionError::Capacity);
    drop(first);
    assert!(admission.try_acquire().is_ok());
}

#[test]
fn blocking_admission_closes_permanently_and_distinguishes_shutdown() {
    let admission = BlockingAdmission::new(1);
    let held = admission.try_acquire().expect("initial permit");

    admission.close();

    assert_eq!(admission.try_acquire().unwrap_err(), AdmissionError::Closed);
    drop(held);
    assert_eq!(admission.try_acquire().unwrap_err(), AdmissionError::Closed);
    assert!(admission.is_closed());
}

#[test]
fn blocking_overload_is_structured_and_reports_the_active_limit() {
    let response = blocking_overload_response(8).into_json_value();

    assert_eq!(response["error"]["kind"], "server_busy");
    assert_eq!(
        response["error"]["details"]["fields"]["max_blocking_requests"],
        8
    );
}

#[test]
fn shutdown_rejection_is_structured_and_not_reported_as_capacity() {
    let response = server_shutting_down_response().into_json_value();

    assert_eq!(response["error"]["kind"], "server_shutting_down");
    assert_eq!(response["error"]["details"], json!({}));
}

#[test]
fn decode_request_preserves_request_fields() {
    let args = json!({
        "command": "echo hi",
        "payload": ["large-ish", "owned", "args"],
    });
    let request = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "scope": {
            "kind": "sandbox",
            "sandbox_id": "sbox-1"
        },
        "args": args.clone(),
    });

    let parsed = decode_request(request).expect("valid request parses");

    assert_eq!(parsed.op, "exec_command");
    assert_eq!(parsed.request_id, "req-1");
    assert_eq!(
        parsed.scope,
        sandbox_operation_contract::OperationScope::sandbox("sbox-1")
    );
    assert_eq!(parsed.args, args);
}

#[test]
fn decode_request_rejects_missing_scope() {
    let request = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "args": {},
    });

    let response = decode_request(request)
        .expect_err("missing scope rejected")
        .into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    assert_eq!(response["error"]["message"], "scope is required");
}

#[test]
fn decode_request_rejects_non_object_args() {
    let request = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "scope": {
            "kind": "sandbox",
            "sandbox_id": "sbox-1"
        },
        "args": "not an object",
    });

    let response = decode_request(request)
        .expect_err("non-object args rejected")
        .into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    assert_eq!(response["error"]["message"], "args must be an object");
}

#[test]
fn sandbox_daemon_ready_echoes_configured_sandbox_id() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "sandbox_daemon_ready",
        "docker-readiness",
        sandbox_operation_contract::OperationScope::sandbox("sbox-1"),
        json!({}),
    );

    let response = daemon_readiness_response(Some("sbox-1"), &request).into_json_value();

    assert_eq!(response["status"], "ready");
    assert_eq!(response["sandbox_id"], "sbox-1");
    assert_eq!(response["daemon"], "sandbox-daemon");
    assert!(response.get("error").is_none());
}

#[test]
fn sandbox_daemon_ready_echoes_request_scope_when_unconfigured() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "sandbox_daemon_ready",
        "docker-readiness",
        sandbox_operation_contract::OperationScope::sandbox("sbox-9"),
        json!({}),
    );

    let response = daemon_readiness_response(None, &request).into_json_value();

    assert_eq!(response["status"], "ready");
    assert_eq!(response["sandbox_id"], "sbox-9");
}

#[test]
fn sandbox_daemon_ready_rejects_sandbox_id_mismatch() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "sandbox_daemon_ready",
        "docker-readiness",
        sandbox_operation_contract::OperationScope::sandbox("requested"),
        json!({}),
    );

    let response = daemon_readiness_response(Some("configured"), &request).into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    let message = response["error"]["message"].as_str().expect("message");
    assert!(message.contains("configured"));
    assert!(message.contains("requested"));
    assert!(response.get("status").is_none());
}

#[test]
fn sandbox_daemon_ready_requires_sandbox_scope() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "sandbox_daemon_ready",
        "docker-readiness",
        sandbox_operation_contract::OperationScope::system(),
        json!({}),
    );

    let response = daemon_readiness_response(None, &request).into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    assert!(response.get("status").is_none());
}

#[test]
fn strip_tcp_auth_accepts_matching_token_and_removes_field() {
    let value = json!({
        "op": "sandbox_daemon_ready",
        "_sandbox_daemon_auth_token": "tok-1",
    });

    let stripped = strip_tcp_auth(Some("tok-1"), value).expect("matching token authorizes");

    assert_eq!(stripped["op"], "sandbox_daemon_ready");
    assert!(
        stripped.get("_sandbox_daemon_auth_token").is_none(),
        "auth token must be stripped before dispatch"
    );
}

#[test]
fn strip_tcp_auth_rejects_mismatched_token() {
    let value = json!({"_sandbox_daemon_auth_token": "wrong"});

    let error = strip_tcp_auth(Some("tok-1"), value).expect_err("mismatched token rejected");

    assert!(matches!(error, SandboxDaemonError::Unauthorized));
}

#[test]
fn strip_tcp_auth_rejects_missing_token() {
    let value = json!({"op": "sandbox_daemon_ready"});

    let error = strip_tcp_auth(Some("tok-1"), value).expect_err("missing token rejected");

    assert!(matches!(error, SandboxDaemonError::Unauthorized));
}

#[test]
fn strip_tcp_auth_passes_through_when_unconfigured() {
    let value = json!({"op": "sandbox_daemon_ready", "_sandbox_daemon_auth_token": "anything"});

    let passed = strip_tcp_auth(None, value).expect("no configured token authorizes");

    assert_eq!(passed["op"], "sandbox_daemon_ready");
}

#[test]
fn daemon_scope_rejects_system_requests() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "exec_command",
        "req-1",
        sandbox_operation_contract::OperationScope::system(),
        json!({}),
    );

    let response = validate_daemon_scope(None, &request)
        .expect_err("system scope rejected")
        .into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    assert_eq!(
        response["error"]["message"],
        "daemon requests require sandbox scope"
    );
}

#[test]
fn daemon_scope_rejects_a_different_configured_sandbox() {
    let request = sandbox_operation_contract::OperationRequest::new(
        "exec_command",
        "req-1",
        sandbox_operation_contract::OperationScope::sandbox("other-sandbox"),
        json!({}),
    );

    let response = validate_daemon_scope(Some("bound-sandbox"), &request)
        .expect_err("cross-sandbox request rejected")
        .into_json_value();

    assert_eq!(response["error"]["kind"], "invalid_request");
    assert_eq!(
        response["error"]["details"]["fields"]["configured_sandbox_id"],
        "bound-sandbox"
    );
    assert_eq!(
        response["error"]["details"]["fields"]["requested_sandbox_id"],
        "other-sandbox"
    );
}
