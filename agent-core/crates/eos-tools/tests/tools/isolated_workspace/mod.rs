#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_sandbox_port::{DaemonOp, SandboxPortError, SandboxTransport};
use eos_types::{JsonObject, SandboxId};
use serde_json::{json, Value};

use crate::support::metadata;
use crate::tools::{CallerScope, SandboxToolService, SkillToolService};
use crate::{ToolName, ToolRegistry};

#[derive(Debug, Clone)]
struct Call {
    op: DaemonOp,
    payload: JsonObject,
}

#[derive(Debug)]
struct RecordingTransport {
    calls: Mutex<Vec<Call>>,
    response: JsonObject,
    error: Option<SandboxPortError>,
}

impl RecordingTransport {
    fn ok(response: Value) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            response: object(response),
            error: None,
        })
    }

    fn err(error: SandboxPortError) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            response: JsonObject::new(),
            error: Some(error),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl SandboxTransport for RecordingTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        self.calls.lock().unwrap().push(Call { op, payload });
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(self.response.clone())
    }
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

fn obj(pairs: &[(&str, Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn registry(transport: Arc<dyn SandboxTransport>) -> ToolRegistry {
    crate::tools::build_default_registry_with_services(
        &crate::tools::repo_tools_config(),
        &CallerScope::default(),
        SandboxToolService::new(transport),
        None,
        None,
        None,
        None,
        None,
        SkillToolService::new(Arc::new(eos_skills::SkillRegistry::new())),
    )
}

fn ctx() -> crate::ExecutionMetadata {
    let mut ctx = metadata();
    ctx.sandbox_id = Some("sb-1".parse().unwrap());
    ctx
}

async fn execute(
    registry: &ToolRegistry,
    name: ToolName,
    input: JsonObject,
) -> crate::ToolResult {
    registry
        .get(name)
        .expect("registered")
        .executor()
        .execute(&input, &ctx())
        .await
        .expect("tool execution")
}

#[tokio::test]
async fn enter_isolated_workspace_uses_default_layer_stack_root() {
    let transport = RecordingTransport::ok(json!({"success": true}));
    let registry = registry(transport.clone());

    let res = execute(
        &registry,
        ToolName::EnterIsolatedWorkspace,
        JsonObject::new(),
    )
    .await;

    assert!(!res.is_error, "{res:?}");
    let calls = transport.calls();
    assert_eq!(calls[0].op, DaemonOp::IsolatedWorkspaceEnter);
    assert_eq!(
        calls[0].payload["layer_stack_root"],
        json!("/eos/state/layer-stack")
    );
}

#[tokio::test]
async fn enter_isolated_workspace_forwards_explicit_layer_stack_root() {
    let transport = RecordingTransport::ok(json!({"success": true}));
    let registry = registry(transport.clone());

    let res = execute(
        &registry,
        ToolName::EnterIsolatedWorkspace,
        obj(&[("layer_stack_root", json!("/custom/layers"))]),
    )
    .await;

    assert!(!res.is_error, "{res:?}");
    let calls = transport.calls();
    assert_eq!(calls[0].op, DaemonOp::IsolatedWorkspaceEnter);
    assert_eq!(calls[0].payload["layer_stack_root"], json!("/custom/layers"));
}

#[tokio::test]
async fn enter_isolated_workspace_renders_success_payload() {
    let transport = RecordingTransport::ok(json!({
        "success": true,
        "manifest_version": "v2",
        "manifest_root_hash": "hash-123",
    }));
    let registry = registry(transport);

    let res = execute(
        &registry,
        ToolName::EnterIsolatedWorkspace,
        JsonObject::new(),
    )
    .await;

    assert!(!res.is_error, "{res:?}");
    let payload: Value = serde_json::from_str(&res.output).unwrap();
    assert_eq!(payload["success"], json!(true));
    assert_eq!(payload["manifest_version"], json!("v2"));
    assert_eq!(payload["manifest_root_hash"], json!("hash-123"));
    assert!(payload["error"].is_null());
}

#[tokio::test]
async fn enter_isolated_workspace_renders_api_failure_as_tool_error() {
    let transport = RecordingTransport::err(SandboxPortError::transport(
        Some("already_active".to_owned()),
        "isolated workspace already active",
    ));
    let registry = registry(transport);

    let res = execute(
        &registry,
        ToolName::EnterIsolatedWorkspace,
        JsonObject::new(),
    )
    .await;

    assert!(res.is_error);
    let payload: Value = serde_json::from_str(&res.output).unwrap();
    assert_eq!(payload["success"], json!(false));
    assert_eq!(payload["error"]["kind"], json!("already_active"));
    assert_eq!(
        payload["error"]["message"],
        json!("isolated workspace already active")
    );
}

#[tokio::test]
async fn exit_isolated_workspace_uses_default_grace() {
    let transport = RecordingTransport::ok(json!({"success": true}));
    let registry = registry(transport.clone());

    let res = execute(&registry, ToolName::ExitIsolatedWorkspace, JsonObject::new()).await;

    assert!(!res.is_error, "{res:?}");
    let calls = transport.calls();
    assert_eq!(calls[0].op, DaemonOp::IsolatedWorkspaceExit);
    assert_eq!(calls[0].payload["grace_s"], json!(5.0));
}

#[tokio::test]
async fn exit_isolated_workspace_forwards_explicit_grace() {
    let transport = RecordingTransport::ok(json!({"success": true}));
    let registry = registry(transport.clone());

    let res = execute(
        &registry,
        ToolName::ExitIsolatedWorkspace,
        obj(&[("grace_s", json!(0.25))]),
    )
    .await;

    assert!(!res.is_error, "{res:?}");
    let calls = transport.calls();
    assert_eq!(calls[0].op, DaemonOp::IsolatedWorkspaceExit);
    assert_eq!(calls[0].payload["grace_s"], json!(0.25));
}

#[tokio::test]
async fn exit_isolated_workspace_renders_success_payload() {
    let transport = RecordingTransport::ok(json!({
        "success": true,
        "evicted_upperdir_bytes": 4096,
        "lifetime_s": 12.5,
        "phases_ms": {"drain": 1.25, "teardown": 2.5},
    }));
    let registry = registry(transport);

    let res = execute(&registry, ToolName::ExitIsolatedWorkspace, JsonObject::new()).await;

    assert!(!res.is_error, "{res:?}");
    let payload: Value = serde_json::from_str(&res.output).unwrap();
    assert_eq!(payload["success"], json!(true));
    assert_eq!(payload["evicted_upperdir_bytes"], json!(4096));
    assert_eq!(payload["lifetime_s"], json!(12.5));
    assert_eq!(payload["phases_ms"]["drain"], json!(1.25));
    assert!(payload["error"].is_null());
}

#[tokio::test]
async fn exit_isolated_workspace_renders_api_failure_as_tool_error() {
    let transport = RecordingTransport::err(SandboxPortError::decode("bad lifecycle payload"));
    let registry = registry(transport);

    let res = execute(&registry, ToolName::ExitIsolatedWorkspace, JsonObject::new()).await;

    assert!(res.is_error);
    let payload: Value = serde_json::from_str(&res.output).unwrap();
    assert_eq!(payload["success"], json!(false));
    assert_eq!(payload["error"]["kind"], json!("decode_error"));
    assert_eq!(payload["error"]["message"], json!("bad lifecycle payload"));
}
