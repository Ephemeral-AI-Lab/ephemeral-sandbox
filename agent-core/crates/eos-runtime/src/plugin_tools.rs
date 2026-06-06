//! Runtime binding for catalog plugin tools.
//!
//! `eos-plugin-catalog` owns the declared model-facing specs. This module binds
//! those specs into real `eos-tools` executors that ensure the daemon manifest
//! for built-in plugin runtimes and then dispatch dynamic `plugin.<plugin>.<op>`
//! daemon operations.

use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::ToolSpec;
use eos_plugin_catalog::{plugin_package_descriptor, plugin_tool_specs, PluginToolSpec};
use eos_sandbox_api::{
    Intent, PluginDispatchRequest, PluginPackageDescriptor, PluginPackageEnsureRequest,
    SandboxRequestBase,
};
use eos_tools::{
    ExecutionMetadata, OutputShape, RegisteredTool, SandboxToolService, ToolError, ToolExecutor,
    ToolIntent, ToolKey, ToolRegistry, ToolResult,
};
use eos_types::JsonObject;
use serde_json::Value;

const PLUGIN_DISPATCH_TIMEOUT_S: u32 = 150;
const PLUGIN_ENSURE_TIMEOUT_S: u32 = 150;

/// Register every built-in plugin catalog tool into `registry`.
pub(crate) fn register_plugin_tools(
    registry: &mut ToolRegistry,
    sandbox_service: &SandboxToolService,
) {
    for spec in plugin_tool_specs() {
        if let Some(tool) = registered_plugin_tool(spec, sandbox_service.clone()) {
            registry.register(tool);
        }
    }
}

fn registered_plugin_tool(
    spec: PluginToolSpec,
    sandbox_service: SandboxToolService,
) -> Option<RegisteredTool> {
    let name = spec.name.as_str().to_owned();
    let parsed_name = split_plugin_tool_name(&name);
    let package = parsed_name
        .as_ref()
        .and_then(|(plugin_id, _)| plugin_package_descriptor(plugin_id))?;
    let input_schema = match serde_json::to_value(spec.input_schema) {
        Ok(Value::Object(map)) => map,
        _ => JsonObject::new(),
    };
    let tool_spec = ToolSpec::new(name.clone(), spec.description, input_schema, None);
    Some(RegisteredTool::new(
        ToolKey::dynamic(name),
        ToolIntent::from(spec.intent),
        false,
        tool_spec,
        OutputShape::Text,
        Arc::new(PluginToolExecutor {
            parsed_name,
            intent: spec.intent,
            package,
            service: sandbox_service,
        }),
    ))
}

fn split_plugin_tool_name(name: &str) -> Option<(String, String)> {
    split_plugin_tool_name_parts(name)
        .map(|(plugin_id, op_name)| (plugin_id.to_owned(), op_name.to_owned()))
}

fn split_plugin_tool_name_parts(name: &str) -> Option<(&str, &str)> {
    name.split_once('.')
        .filter(|(plugin_id, op_name)| !plugin_id.is_empty() && !op_name.is_empty())
}

#[derive(Debug)]
struct PluginToolExecutor {
    parsed_name: Option<(String, String)>,
    intent: Intent,
    package: PluginPackageDescriptor,
    service: SandboxToolService,
}

#[async_trait]
impl ToolExecutor for PluginToolExecutor {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let Some((plugin_id, op_name)) = &self.parsed_name else {
            return Err(ToolError::Internal(
                "catalog plugin tool name must be <plugin>.<op>".to_owned(),
            ));
        };
        let sandbox_id = ctx.require_sandbox_id()?;
        let agent_run_id = ctx.require_agent_run_id()?;
        let base = SandboxRequestBase::new(
            agent_run_id.as_str(),
            format!("plugin {plugin_id}.{op_name}"),
            ctx.sandbox_invocation_id.clone(),
        );
        ensure_plugin_runtime(&self.service, &self.package, &base, ctx).await?;
        let response = eos_sandbox_api::plugin_dispatch(
            &*self.service.transport(),
            sandbox_id,
            PluginDispatchRequest {
                base,
                plugin_id: plugin_id.clone(),
                op_name: op_name.clone(),
                intent: self.intent,
                workspace_root: ctx.workspace_root.clone(),
                args: input.clone(),
                timeout_s: PLUGIN_DISPATCH_TIMEOUT_S,
            },
        )
        .await?;
        Ok(plugin_result(&response))
    }
}

async fn ensure_plugin_runtime(
    service: &SandboxToolService,
    package: &PluginPackageDescriptor,
    base: &SandboxRequestBase,
    ctx: &ExecutionMetadata,
) -> Result<(), ToolError> {
    let sandbox_id = ctx.require_sandbox_id()?;
    eos_sandbox_api::ensure_plugin_package(
        &*service.transport(),
        sandbox_id,
        PluginPackageEnsureRequest {
            base: base.clone(),
            workspace_root: ctx.workspace_root.clone(),
            package: package.clone(),
            start_services: true,
            timeout_s: PLUGIN_ENSURE_TIMEOUT_S,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
fn json_object(value: Value) -> JsonObject {
    match value {
        Value::Object(object) => object,
        _ => unreachable!("plugin manifest literal must be a JSON object"),
    }
}

fn plugin_result(response: &JsonObject) -> ToolResult {
    let is_error = response.get("success") == Some(&Value::Bool(false));
    let output = serde_json::to_string(response)
        .unwrap_or_else(|err| format!(r#"{{"success":false,"error":"{err}"}}"#));
    if is_error {
        ToolResult::error(output)
    } else {
        ToolResult::ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use eos_sandbox_api::{DaemonOp, SandboxApiError, SandboxTransport};
    use eos_types::SandboxId;
    use serde_json::json;

    #[test]
    fn registers_lsp_plugin_tools() {
        let mut registry = ToolRegistry::new();
        register_plugin_tools(
            &mut registry,
            &SandboxToolService::new(Arc::new(RecordingTransport::default())),
        );
        let hover = registry.get_wire("lsp.hover").expect("hover registered");
        assert_eq!(hover.name.as_str(), "lsp.hover");
        assert_eq!(hover.intent, ToolIntent::ReadOnly);
        assert!(!hover.is_terminal);
        assert!(registry.get_wire("lsp.rename").is_some());
    }

    #[tokio::test]
    async fn lsp_executor_ensures_package_before_dispatch() {
        let transport = Arc::new(RecordingTransport::default());
        let ctx = metadata_with(transport.clone());
        let package = plugin_package_descriptor("lsp").expect("lsp package");
        let executor = PluginToolExecutor {
            parsed_name: Some(("lsp".to_owned(), "hover".to_owned())),
            intent: Intent::ReadOnly,
            package: package.clone(),
            service: SandboxToolService::new(transport.clone()),
        };
        let input = json_object(json!({
            "file_path": "src/main.py",
            "line": 2,
            "character": 4
        }));

        let _result = executor.execute(&input, &ctx).await.expect("execute");

        let calls = transport.calls.lock().expect("calls lock").clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].op, "api.plugin.ensure");
        assert_eq!(calls[0].timeout_s, PLUGIN_ENSURE_TIMEOUT_S);
        assert_eq!(
            calls[0].payload.get("workspace_root"),
            Some(&json!("/repo"))
        );
        assert_eq!(calls[0].payload.get("start_services"), Some(&json!(true)));
        assert_eq!(calls[0].payload.get("invocation_id"), Some(&json!("inv-1")));

        let manifest = calls[0].payload.get("manifest").expect("manifest");
        assert_eq!(
            manifest.get("plugin_id"),
            Some(&json!(package.manifest.plugin_id))
        );
        assert_eq!(
            manifest.get("plugin_digest"),
            Some(&json!(package.manifest.plugin_digest))
        );
        let command = manifest
            .get("services")
            .and_then(Value::as_array)
            .and_then(|services| services.first())
            .and_then(|service| service.get("command"))
            .and_then(Value::as_array)
            .expect("service command");
        assert_eq!(command, &vec![json!("./ppc_service.py")]);
        assert!(command
            .iter()
            .all(|part| !part.as_str().unwrap_or_default().starts_with("/eos/")));
        let operations = manifest
            .get("operations")
            .and_then(Value::as_array)
            .expect("operations");
        let catalog_lsp_tool_count = plugin_tool_specs()
            .into_iter()
            .filter(|spec| {
                split_plugin_tool_name_parts(spec.name.as_str())
                    .is_some_and(|(plugin_id, _)| plugin_id == "lsp")
            })
            .count();
        assert_eq!(operations.len(), catalog_lsp_tool_count);
        assert!(operations.iter().any(|operation| {
            operation.get("op_name") == Some(&json!("rename"))
                && operation.get("intent") == Some(&json!("write_allowed"))
                && operation.get("service_id") == Some(&json!("pyright"))
                && operation.get("auto_workspace_overlay") == Some(&json!(false))
        }));

        assert_eq!(calls[1].op, "plugin.lsp.hover");
        assert_eq!(calls[1].timeout_s, PLUGIN_DISPATCH_TIMEOUT_S);
        assert_eq!(
            calls[1].payload.get("file_path"),
            Some(&json!("src/main.py"))
        );
        assert_eq!(calls[1].payload.get("intent"), Some(&json!("read_only")));
        assert_eq!(
            calls[1].payload.get("workspace_root"),
            Some(&json!("/repo"))
        );
    }

    #[derive(Debug, Clone)]
    struct RecordedCall {
        op: String,
        payload: JsonObject,
        timeout_s: u32,
    }

    #[derive(Debug, Default)]
    struct RecordingTransport {
        calls: Mutex<Vec<RecordedCall>>,
    }

    #[async_trait]
    impl SandboxTransport for RecordingTransport {
        async fn call(
            &self,
            _sandbox_id: &SandboxId,
            op: DaemonOp,
            payload: JsonObject,
            timeout_s: u32,
        ) -> Result<JsonObject, SandboxApiError> {
            self.calls.lock().expect("calls lock").push(RecordedCall {
                op: op.as_wire().to_owned(),
                payload,
                timeout_s,
            });
            Ok(json_object(json!({"success": true})))
        }

        async fn call_dynamic(
            &self,
            _sandbox_id: &SandboxId,
            op: &str,
            payload: JsonObject,
            timeout_s: u32,
        ) -> Result<JsonObject, SandboxApiError> {
            self.calls.lock().expect("calls lock").push(RecordedCall {
                op: op.to_owned(),
                payload,
                timeout_s,
            });
            Ok(json_object(json!({"success": true, "value": "ok"})))
        }
    }

    fn metadata_with(_transport: Arc<dyn SandboxTransport>) -> ExecutionMetadata {
        ExecutionMetadata {
            sandbox_id: Some("sandbox-1".parse().expect("sandbox id")),
            agent_run_id: Some("agent-run-1".parse().expect("agent run id")),
            agent_name: "tester".to_owned(),
            request_id: None,
            task_id: None,
            attempt_id: None,
            workflow_id: None,
            tool_use_id: None,
            sandbox_invocation_id: Some("inv-1".parse().expect("invocation id")),
            is_isolated_workspace_mode: false,
            workspace_root: "/repo".to_owned(),
            conversation: Arc::from(Vec::new()),
        }
    }
}
