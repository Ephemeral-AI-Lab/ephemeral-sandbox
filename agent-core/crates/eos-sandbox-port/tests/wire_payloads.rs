//! Outbound daemon-payload contracts for the `tool_api` helpers.
//!
//! `parse.rs` (response decoding) is well-covered; the **request** side — the
//! hand-built JSON payload each helper sends, the op it targets, and the timeout
//! — was only asserted for the isolated-workspace verbs. These tests record the
//! outbound call via a [`RecordingTransport`] and pin the payload for the
//! file/command/control/plugin helpers, so a dropped or renamed field (the kind
//! of drift that silently breaks the daemon wire) fails a test.
#![allow(clippy::expect_used)]

use std::sync::Mutex;

use async_trait::async_trait;
use eos_sandbox_port::{
    cancel, cancel_command_session, collect_command_completions, command_session_count,
    exec_command, exec_dispatch_timeout, exec_stdin, heartbeat, inflight_count, isolated_active,
    plugin_dispatch, plugin_ensure, read_command_progress, read_file, write_file,
    CommandSessionCancelRequest, DaemonOp, ExecCommandRequest, ExecStdinRequest, Intent,
    PluginDependencyScope, PluginDispatchRequest, PluginEnsureRequest, PluginManifestDescriptor,
    PluginOperationDescriptor, PluginPackageContract, PluginRefreshStrategy,
    PluginServiceDescriptor, PluginServiceMode, ReadCommandProgressRequest, ReadFileRequest,
    SandboxPortError, SandboxRequestBase, SandboxTransport, WriteFileRequest, READ_FILE_TIMEOUT_S,
};
use eos_types::{CommandSessionId, InvocationId, JsonObject, SandboxId};
use serde_json::{json, Value};

/// Control-RPC timeout (`control::CONTROL_TIMEOUT_S`, not publicly exported).
const CONTROL_TIMEOUT_S: u32 = 15;

enum Recorded {
    Typed {
        op: DaemonOp,
        payload: JsonObject,
        timeout_s: u32,
    },
    Dynamic {
        op: String,
        payload: JsonObject,
        timeout_s: u32,
    },
}

/// Records every outbound call and returns one canned response.
struct RecordingTransport {
    calls: Mutex<Vec<Recorded>>,
    response: JsonObject,
}

impl RecordingTransport {
    fn new(response: Value) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response: object(response),
        }
    }

    fn typed(&self) -> (DaemonOp, JsonObject, u32) {
        match self
            .calls
            .lock()
            .expect("calls lock")
            .first()
            .expect("a call")
        {
            Recorded::Typed {
                op,
                payload,
                timeout_s,
            } => (*op, payload.clone(), *timeout_s),
            Recorded::Dynamic { .. } => panic!("expected a typed call, got a dynamic one"),
        }
    }

    fn dynamic(&self) -> (String, JsonObject, u32) {
        match self
            .calls
            .lock()
            .expect("calls lock")
            .first()
            .expect("a call")
        {
            Recorded::Dynamic {
                op,
                payload,
                timeout_s,
            } => (op.clone(), payload.clone(), *timeout_s),
            Recorded::Typed { .. } => panic!("expected a dynamic call, got a typed one"),
        }
    }
}

#[async_trait]
impl SandboxTransport for RecordingTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(Recorded::Typed {
                op,
                payload,
                timeout_s,
            });
        Ok(self.response.clone())
    }

    async fn call_dynamic(
        &self,
        _sandbox_id: &SandboxId,
        op: &str,
        payload: JsonObject,
        timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(Recorded::Dynamic {
                op: op.to_owned(),
                payload,
                timeout_s,
            });
        Ok(self.response.clone())
    }
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

fn sandbox() -> SandboxId {
    "sb-1".parse().expect("sandbox id")
}

fn base() -> SandboxRequestBase {
    SandboxRequestBase {
        caller_id: "agent-1".to_owned(),
        description: String::new(),
        invocation_id: None,
    }
}

// ---- file helpers ---------------------------------------------------------

#[tokio::test]
async fn read_file_payload_has_path_and_caller_but_omits_description() {
    let transport = RecordingTransport::new(json!({
        "success": true, "content": "hi", "exists": true, "encoding": "utf-8"
    }));
    read_file(
        &transport,
        &sandbox(),
        &ReadFileRequest {
            base: base(),
            path: "src/x.rs".to_owned(),
        },
    )
    .await
    .expect("read");

    let (op, payload, timeout_s) = transport.typed();
    assert_eq!(op, DaemonOp::ReadFile);
    assert_eq!(payload["caller_id"], json!("agent-1"));
    assert_eq!(payload["path"], json!("src/x.rs"));
    assert_eq!(timeout_s, READ_FILE_TIMEOUT_S);
    // read_file does not send `description`, unlike the mutation/lifecycle verbs
    // (write/edit/isolated) that carry it. `description` is the optional audit
    // label on the shared base; a pure read isn't an audited mutation, so its
    // absence is consistent with intent. Pinned so the payload shape can't drift
    // silently (update if reads ever start sending a description).
    assert!(
        !payload.contains_key("description"),
        "a read carries no audit description, unlike the mutation verbs"
    );
}

#[tokio::test]
async fn write_file_payload_includes_content_description_and_overwrite() {
    let transport = RecordingTransport::new(json!({"success": true, "status": "ok"}));
    write_file(
        &transport,
        &sandbox(),
        &WriteFileRequest {
            base: base(),
            path: "a.txt".to_owned(),
            content: "data".to_owned(),
            overwrite: false,
        },
    )
    .await
    .expect("write");

    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::WriteFile);
    assert_eq!(payload["path"], json!("a.txt"));
    assert_eq!(payload["content"], json!("data"));
    assert_eq!(payload["overwrite"], json!(false));
    // Blank base description falls back to "write {path}".
    assert_eq!(payload["description"], json!("write a.txt"));
}

// ---- command-session helpers ----------------------------------------------

#[tokio::test]
async fn exec_command_payload_includes_set_options_and_omits_unset() {
    let transport = RecordingTransport::new(json!({
        "status": "ok", "output": {"stdout": "", "stderr": ""}
    }));
    exec_command(
        &transport,
        &sandbox(),
        &ExecCommandRequest {
            base: base(),
            cmd: "ls".to_owned(),
            yield_time_ms: Some(500),
            timeout: Some(30),
        },
    )
    .await
    .expect("exec");

    let (op, payload, timeout_s) = transport.typed();
    assert_eq!(op, DaemonOp::ExecCommand);
    assert_eq!(payload["cmd"], json!("ls"));
    assert_eq!(payload["yield_time_ms"], json!(500));
    assert_eq!(payload["timeout"], json!(30));
    assert!(
        !payload.contains_key("max_output_tokens"),
        "an unset option is omitted from the payload"
    );
    // Dispatch timeout is derived from the command timeout.
    assert_eq!(timeout_s, exec_dispatch_timeout(Some(30)));
}

#[tokio::test]
async fn exec_stdin_payload_is_input_only_and_keeps_invocation() {
    let session: CommandSessionId = "cs-1".parse().expect("cs id");
    let transport = RecordingTransport::new(json!({"status": "ok", "output": {}}));
    exec_stdin(
        &transport,
        &sandbox(),
        &ExecStdinRequest {
            base: SandboxRequestBase {
                caller_id: "agent-1".to_owned(),
                description: String::new(),
                invocation_id: Some("inv-9".parse().expect("inv id")),
            },
            command_session_id: session.clone(),
            chars: "y\n".to_owned(),
            yield_time_ms: Some(250),
        },
    )
    .await
    .expect("stdin");
    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::ExecStdin);
    assert_eq!(payload["command_session_id"], json!("cs-1"));
    assert_eq!(payload["chars"], json!("y\n"));
    assert_eq!(payload["yield_time_ms"], json!(250));
    assert_eq!(payload["invocation_id"], json!("inv-9"));
    assert!(!payload.contains_key("terminate"));
    assert!(!payload.contains_key("max_output_tokens"));
}

#[tokio::test]
async fn read_command_progress_payload_targets_tail_snapshot_op() {
    let session: CommandSessionId = "cs-1".parse().expect("cs id");
    let transport = RecordingTransport::new(json!({"status": "running", "output": {}}));
    read_command_progress(
        &transport,
        &sandbox(),
        &ReadCommandProgressRequest {
            base: base(),
            command_session_id: session,
            last_n_lines: 25,
        },
    )
    .await
    .expect("read progress");
    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::CommandReadProgress);
    assert_eq!(payload["command_session_id"], json!("cs-1"));
    assert_eq!(payload["last_n_lines"], json!(25));
}

#[tokio::test]
async fn cancel_command_session_payload_targets_the_session() {
    let transport = RecordingTransport::new(json!({"status": "ok", "output": {}}));
    cancel_command_session(
        &transport,
        &sandbox(),
        &CommandSessionCancelRequest {
            base: base(),
            command_session_id: "cs-2".parse().expect("cs id"),
        },
    )
    .await
    .expect("cancel");
    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::CommandCancel);
    assert_eq!(payload["command_session_id"], json!("cs-2"));
}

#[tokio::test]
async fn collect_command_completions_builds_payload_and_filters_non_objects() {
    let transport = RecordingTransport::new(json!({
        "completions": [{"command_session_id": "c1"}, "ignore", 7]
    }));
    let got = collect_command_completions(
        &transport,
        &sandbox(),
        "agent-1",
        &["c1".to_owned(), "c2".to_owned()],
    )
    .await
    .expect("collect");

    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::CommandCollectCompleted);
    assert_eq!(payload["caller_id"], json!("agent-1"));
    assert_eq!(payload["command_session_ids"], json!(["c1", "c2"]));
    // Only object entries survive; the string and number are dropped.
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["command_session_id"], json!("c1"));
}

// ---- control RPCs ---------------------------------------------------------

#[tokio::test]
async fn control_cancel_and_heartbeat_build_payloads() {
    let transport = RecordingTransport::new(json!({}));
    cancel(
        &transport,
        &sandbox(),
        &"inv-1".parse::<InvocationId>().expect("inv id"),
    )
    .await
    .expect("cancel");
    let (op, payload, timeout_s) = transport.typed();
    assert_eq!(op, DaemonOp::InvocationCancel);
    assert_eq!(payload["invocation_id"], json!("inv-1"));
    assert_eq!(timeout_s, CONTROL_TIMEOUT_S);

    let transport = RecordingTransport::new(json!({}));
    heartbeat(
        &transport,
        &sandbox(),
        &["inv-1".parse().expect("a"), "inv-2".parse().expect("b")],
    )
    .await
    .expect("heartbeat");
    let (op, payload, _) = transport.typed();
    assert_eq!(op, DaemonOp::InvocationHeartbeat);
    assert_eq!(payload["invocation_ids"], json!(["inv-1", "inv-2"]));
}

#[tokio::test]
async fn control_counts_read_count_and_default_to_zero() {
    let transport = RecordingTransport::new(json!({"count": 3}));
    assert_eq!(
        inflight_count(&transport, &sandbox(), "agent-1")
            .await
            .expect("count"),
        3
    );
    let (op, payload, timeout_s) = transport.typed();
    assert_eq!(op, DaemonOp::InflightCount);
    assert_eq!(payload["caller_id"], json!("agent-1"));
    assert_eq!(timeout_s, CONTROL_TIMEOUT_S);

    // A response without a `count` defaults to 0.
    let transport = RecordingTransport::new(json!({}));
    assert_eq!(
        command_session_count(&transport, &sandbox(), "agent-1")
            .await
            .expect("count"),
        0
    );
    assert_eq!(transport.typed().0, DaemonOp::CommandSessionCount);
}

#[tokio::test]
async fn isolated_active_defaults_false_when_open_key_absent() {
    let transport = RecordingTransport::new(json!({"open": true}));
    assert!(isolated_active(&transport, &sandbox(), "agent-1")
        .await
        .expect("active"));
    assert_eq!(transport.typed().0, DaemonOp::IsolatedWorkspaceStatus);

    // No `open` key (e.g. the no-pipeline error payload) -> false (fail-safe).
    let transport = RecordingTransport::new(json!({"error": "no pipeline"}));
    assert!(!isolated_active(&transport, &sandbox(), "agent-1")
        .await
        .expect("inactive"));
}

// ---- plugin helpers -------------------------------------------------------

#[tokio::test]
async fn plugin_dispatch_uses_dynamic_op_name_and_merges_args() {
    let transport = RecordingTransport::new(json!({}));
    let mut args = JsonObject::new();
    args.insert("symbol".to_owned(), json!("foo"));
    plugin_dispatch(
        &transport,
        &sandbox(),
        PluginDispatchRequest {
            base: base(),
            plugin_id: "lsp".to_owned(),
            op_name: "hover".to_owned(),
            intent: Intent::ReadOnly,
            workspace_root: "/repo".to_owned(),
            args,
            timeout_s: 20,
        },
    )
    .await
    .expect("dispatch");

    let (op, payload, timeout_s) = transport.dynamic();
    assert_eq!(op, "plugin.lsp.hover");
    assert_eq!(timeout_s, 20);
    assert_eq!(payload["caller_id"], json!("agent-1"));
    assert_eq!(payload["symbol"], json!("foo")); // model args merged in
    assert_eq!(payload["intent"], json!(Intent::ReadOnly.as_wire()));
    assert_eq!(payload["workspace_root"], json!("/repo"));
}

#[tokio::test]
async fn plugin_ensure_builds_manifest_payload() {
    let transport = RecordingTransport::new(json!({}));
    plugin_ensure(
        &transport,
        &sandbox(),
        PluginEnsureRequest {
            base: base(),
            workspace_root: "/repo".to_owned(),
            manifest: sample_manifest(),
            staged_package_root: Some("/staged".to_owned()),
            start_services: true,
            timeout_s: 30,
        },
    )
    .await
    .expect("ensure");

    let (op, payload, timeout_s) = transport.typed();
    assert_eq!(op, DaemonOp::PluginEnsure);
    assert_eq!(timeout_s, 30);
    assert_eq!(payload["workspace_root"], json!("/repo"));
    assert_eq!(payload["staged_package_root"], json!("/staged"));
    assert_eq!(payload["start_services"], json!(true));
    assert_eq!(payload["manifest"]["plugin_id"], json!("lsp"));
    assert_eq!(
        payload["manifest"]["operations"][0]["op_name"],
        json!("hover")
    );
}

#[test]
fn plugin_descriptor_serializes_with_snake_case_enums() {
    let value = serde_json::to_value(sample_manifest()).expect("encode manifest");
    assert_eq!(
        value["services"][0]["service_mode"],
        json!("workspace_snapshot_refresh")
    );
    assert_eq!(
        value["services"][0]["refresh_strategy"],
        json!("remount_workspace_and_notify")
    );
    assert_eq!(
        value["package"]["dependency_scope"],
        json!("package_digest")
    );
}

fn sample_manifest() -> PluginManifestDescriptor {
    PluginManifestDescriptor {
        plugin_id: "lsp".to_owned(),
        plugin_version: "1.0".to_owned(),
        plugin_digest: "deadbeef".to_owned(),
        package: PluginPackageContract {
            runtime_dir: "runtime".to_owned(),
            dependency_scope: PluginDependencyScope::PackageDigest,
        },
        setup: None,
        services: vec![PluginServiceDescriptor {
            service_id: "svc".to_owned(),
            service_profile_digest: "abc".to_owned(),
            service_mode: PluginServiceMode::WorkspaceSnapshotRefresh,
            refresh_strategy: PluginRefreshStrategy::RemountWorkspaceAndNotify,
            command: vec!["run".to_owned()],
            working_dir: None,
            ppc_protocol_version: 1,
        }],
        operations: vec![PluginOperationDescriptor {
            op_name: "hover".to_owned(),
            intent: Intent::ReadOnly,
            auto_workspace_overlay: false,
            service_id: Some("svc".to_owned()),
            timeout_ms: Some(1000),
        }],
    }
}
