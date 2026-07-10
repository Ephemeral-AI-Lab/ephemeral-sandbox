//! Squash operation surface: registration without a CLI catalog entry, the
//! minimal output contract, singleflight faults, and the per-session
//! admission gate as the single serializer.

use std::sync::Arc;
use std::time::Duration;

use sandbox_operation_contract::{OperationRequest, OperationScope, OperationScopeKind};
use sandbox_runtime::workspace_session::SweptDisposition;
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_layerstack::{LayerChange, LayerPath, LayerStack};
use sandbox_runtime_workspace::NetworkProfile;
use sandbox_runtime_workspace::WorkspaceSessionId;
use serde_json::json;

mod support;
use support::FakeWorkspaceService;

fn squash_request() -> OperationRequest {
    OperationRequest::new(
        "squash_layerstack",
        "req-squash-test",
        OperationScope::sandbox("sbox-test"),
        json!({}),
    )
}

fn operations_with_real_layerstack() -> (SandboxRuntimeOperations, std::path::PathBuf) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability::Observer::disabled());
    let root = layerstack.layer_stack_root().to_path_buf();
    let services = support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        Arc::new(support::FakeLaunchDriver::new()),
        Arc::clone(&layerstack),
    );
    let operations = SandboxRuntimeOperations::new(
        services.command,
        services.workspace,
        layerstack,
        support::test_file_service(),
    );
    (operations, root)
}

fn publish(root: &std::path::Path, path: &str, content: &str) {
    let mut stack = LayerStack::open(root.to_path_buf()).expect("open stack");
    stack
        .publish_layer(&[LayerChange::Write {
            path: LayerPath::parse(path).expect("path"),
            content: content.as_bytes().to_vec(),
        }])
        .expect("publish");
}

#[test]
fn squash_layerstack_is_internal_and_absent_from_the_public_catalog() {
    assert!(sandbox_runtime::runtime_internal_handler_keys()
        .any(|key| { key == (OperationScopeKind::Sandbox, "squash_layerstack") }));
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
    let encoded = sandbox_operation_contract::catalog_to_value(catalog).to_string();
    assert!(
        !encoded.contains("squash_layerstack"),
        "internal registration must keep the op out of every catalog surface"
    );
}

// Test 17: the result carries exactly manifest_version + squashed_blocks
// (+ faulty_sessions only when non-empty); blocked blocks carry non-empty
// free-form reasons; nothing-to-squash is the state speaking for itself.
#[test]
fn squash_output_contract() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    publish(&root, "b.txt", "2");
    publish(&root, "c.txt", "3");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    let object = value.as_object().expect("result object");
    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        vec!["manifest_version", "squashed_blocks"],
        "no layers, no leases, no no_op, no faulty_sessions when empty"
    );
    assert_eq!(value["manifest_version"], json!(5));
    let blocks = value["squashed_blocks"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1);
    let block = blocks[0].as_object().expect("block object");
    assert!(block["squashed_layer_id"]
        .as_str()
        .expect("id")
        .starts_with('S'));
    assert_eq!(
        block["replaced_layer_ids"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(block["replaced_layers"], json!("reclaimed"));
    assert!(
        !block.contains_key("blocked_reasons"),
        "reclaimed blocks carry no reasons"
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert_eq!(value["manifest_version"], json!(5));
    assert_eq!(
        value["squashed_blocks"].as_array().map(Vec::len),
        Some(0),
        "nothing to squash: empty blocks, no no_op flag"
    );
}

// A leased block reports non-empty blocked_reasons even when no live session
// maps to the pinning lease (the plan-window holder fallback).
#[test]
fn squash_reports_leased_blocks_with_reasons() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    publish(&root, "b.txt", "2");
    publish(&root, "c.txt", "3");
    let stack = LayerStack::open(root.clone()).expect("open stack");
    let lease = stack.acquire_snapshot("pinning-holder").expect("lease");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    let blocks = value["squashed_blocks"].as_array().expect("blocks");
    assert_eq!(
        blocks.len(),
        1,
        "block forms below the lease's newest layer"
    );
    assert_eq!(blocks[0]["replaced_layers"], json!("leased"));
    let reasons = blocks[0]["blocked_reasons"].as_array().expect("reasons");
    assert!(
        !reasons.is_empty(),
        "blocked_reasons is non-empty whenever leased"
    );
    drop(lease);
}

// Singleflight: a second invocation while one is in flight fails cleanly as
// operation_failed — no dedicated error kind exists.
#[test]
fn squash_singleflight_faults_as_operation_failed() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    let mut stack = LayerStack::open(root).expect("open stack");
    let in_flight = stack.squash().expect("first squash holds the flight");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert_eq!(value["error"]["kind"], json!("operation_failed"));
    assert!(value["error"]["message"]
        .as_str()
        .expect("message")
        .contains("already in flight"));
    drop(in_flight);

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert!(
        value.get("error").is_none(),
        "squash succeeds after the flight releases"
    );
}

// Test 9 (deterministic core): the per-session gate is the single
// serializer — a destroy parked inside the gate blocks a concurrent session
// file op until it completes; a remount of an unknown session is a silent
// skip that leaks nothing.
#[test]
fn admission_gate_serializes_destroy_against_file_ops() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(support::workspace_handle(
        "ws-gate",
        "lease-gate",
        std::env::temp_dir().join("squash-gate-ws"),
        NetworkProfile::Shared,
    )));
    let services = support::build_services(Arc::clone(&fake));
    let handler = services
        .workspace
        .create_workspace_session(support::create_request())
        .expect("create session");
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        support::observed_layerstack_service(sandbox_observability::Observer::disabled()),
        support::test_file_service(),
    );

    let (entered, release) = fake.park_next_destroy();
    let destroy_ops = operations.clone();
    let destroy_id = handler.workspace_session_id.0.clone();
    let destroyer = std::thread::spawn(move || {
        sandbox_runtime::dispatch_operation(
            &destroy_ops,
            &OperationRequest::new(
                "destroy_workspace_session",
                "req-destroy-gate",
                OperationScope::sandbox("sbox-test"),
                json!({ "workspace_session_id": destroy_id }),
            ),
        )
    });
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("destroy reached the workspace hook while holding the gate");

    let file_op_workspace = Arc::clone(&services.workspace);
    let file_op_id = handler.workspace_session_id.clone();
    let file_op = std::thread::spawn(move || {
        file_op_workspace.run_file_op(
            &file_op_id,
            sandbox_runtime_workspace::FileRunnerOp::ReadFile {
                rel: "f.txt".to_owned(),
                max_bytes: 16,
            },
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op must wait on the session gate while destroy holds it"
    );

    release.send(()).expect("release the parked destroy");
    let _ = destroyer.join().expect("destroy thread");
    let file_op_result = file_op.join().expect("file op thread");
    assert!(
        matches!(
            file_op_result,
            Err(sandbox_runtime::workspace_session::WorkspaceSessionError::NotFound { .. })
        ),
        "a file op racing the destroy resolves inside the gate and loses cleanly"
    );
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op never runs against the destroyed session"
    );

    let swept = services
        .workspace
        .remount_session(&WorkspaceSessionId("ws-never-existed".to_owned()));
    assert_eq!(swept.disposition, SweptDisposition::SessionGone);
    assert_eq!(services.workspace.gate_entry_count(), 0);
}
