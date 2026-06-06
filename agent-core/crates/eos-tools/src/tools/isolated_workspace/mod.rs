//! Isolated-workspace lifecycle tools.

use std::collections::BTreeMap;

use eos_sandbox_api::{
    EnterIsolatedWorkspaceResult, ExitIsolatedWorkspaceResult, LifecycleError, SandboxApiError,
};
use serde_json::{json, Value};

use crate::{ToolError, ToolResult};

mod enter_isolated_workspace;
mod exit_isolated_workspace;

const DEFAULT_LAYER_STACK_ROOT: &str = "/eos/state/layer-stack";

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    sandbox_service: super::SandboxToolService,
) {
    enter_isolated_workspace::register(registry, config, sandbox_service.clone());
    exit_isolated_workspace::register(registry, config, sandbox_service);
}

fn effective_layer_stack_root(layer_stack_root: &str) -> String {
    if layer_stack_root.is_empty() {
        DEFAULT_LAYER_STACK_ROOT.to_owned()
    } else {
        layer_stack_root.to_owned()
    }
}

fn render_enter_result(result: &EnterIsolatedWorkspaceResult) -> Result<ToolResult, ToolError> {
    render_lifecycle(
        result.base.success,
        &json!({
            "success": result.base.success,
            "manifest_version": result.manifest_version,
            "manifest_root_hash": result.manifest_root_hash,
            "error": lifecycle_error_value(result.base.error.as_ref()),
        }),
    )
}

fn render_exit_result(result: &ExitIsolatedWorkspaceResult) -> Result<ToolResult, ToolError> {
    render_lifecycle(
        result.base.success,
        &json!({
            "success": result.base.success,
            "evicted_upperdir_bytes": result.evicted_upperdir_bytes,
            "lifetime_s": result.lifetime_s,
            "phases_ms": result.phases_ms,
            "error": lifecycle_error_value(result.base.error.as_ref()),
        }),
    )
}

fn render_enter_api_failure(error: &SandboxApiError) -> Result<ToolResult, ToolError> {
    render_lifecycle(
        false,
        &json!({
            "success": false,
            "manifest_version": "",
            "manifest_root_hash": "",
            "error": lifecycle_error_value(Some(&lifecycle_error_from_api(error))),
        }),
    )
}

fn render_exit_api_failure(error: &SandboxApiError) -> Result<ToolResult, ToolError> {
    render_lifecycle(
        false,
        &json!({
            "success": false,
            "evicted_upperdir_bytes": 0,
            "lifetime_s": 0.0,
            "phases_ms": {},
            "error": lifecycle_error_value(Some(&lifecycle_error_from_api(error))),
        }),
    )
}

fn render_lifecycle(success: bool, payload: &Value) -> Result<ToolResult, ToolError> {
    let output = serde_json::to_string_pretty(payload).map_err(|err| {
        ToolError::Internal(format!("failed to serialize lifecycle result: {err}"))
    })?;
    Ok(if success {
        ToolResult::ok(output)
    } else {
        ToolResult::error(output)
    })
}

fn lifecycle_error_value(error: Option<&LifecycleError>) -> Value {
    match error {
        Some(error) => json!({
            "kind": error.kind,
            "message": error.message,
            "details": error.details,
        }),
        None => Value::Null,
    }
}

fn lifecycle_error_from_api(error: &SandboxApiError) -> LifecycleError {
    let fallback = match error {
        SandboxApiError::Decode { .. } => "decode_error",
        _ => "internal_error",
    };
    LifecycleError {
        kind: error.code().unwrap_or(fallback).to_owned(),
        message: error.message().to_owned(),
        details: BTreeMap::new(),
    }
}
