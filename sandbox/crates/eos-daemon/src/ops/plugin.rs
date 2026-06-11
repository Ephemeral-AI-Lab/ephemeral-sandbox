//! Plugin op adapters: caller-family gating plus dispatch into the owned
//! [`crate::services::plugin::PluginRuntime`].

use eos_plugin::host::ensure_args::validate_plugin_caller_fields;
use eos_plugin::PluginError;
use serde_json::Value;

use crate::error::DaemonError;
use crate::runtime::context::DispatchContext;
use crate::runtime::services::Services;

pub(crate) fn op_ensure(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    ensure_plugin_family_allowed(services, args)?;
    services.plugin.op_ensure(args)
}

pub(crate) fn op_status(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    ensure_plugin_family_allowed(services, args)?;
    services.plugin.op_status(args)
}

/// Dispatch a dynamically registered `plugin.*` op after a built-in table miss,
/// or `None` when the op is not plugin-shaped / not registered.
pub(crate) fn dispatch_registered_op(
    op: &str,
    invocation_id: &str,
    args: &Value,
    context: DispatchContext<'_>,
) -> Option<Result<Value, DaemonError>> {
    if !op.starts_with("plugin.") {
        return None;
    }
    let services = match context.require_services() {
        Ok(services) => services,
        Err(err) => return Some(Err(err)),
    };
    // Single caller-family gate for the whole registered-op dispatch chain; the
    // routing below it trusts the already-validated args.
    if let Err(err) = ensure_plugin_family_allowed(services, args) {
        return Some(Err(err));
    }
    services
        .plugin
        .dispatch_registered_op(op, invocation_id, args)
}

/// The plugin caller-family gate: validate the caller fields, then refuse the
/// whole `api.plugin.*` + registered-op family for callers inside an isolated
/// workspace. Composed here so the plugin runtime never reaches into
/// isolated-workspace state.
fn ensure_plugin_family_allowed(services: &Services, args: &Value) -> Result<(), DaemonError> {
    validate_plugin_caller_fields(args)?;
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !caller_id.is_empty() && services.workspace.caller_has_active_handle(caller_id) {
        return Err(DaemonError::Plugin(
            PluginError::ForbiddenInIsolatedWorkspace,
        ));
    }
    Ok(())
}
