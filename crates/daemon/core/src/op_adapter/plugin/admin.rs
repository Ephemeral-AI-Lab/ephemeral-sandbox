use operation::plugin::contract::{PluginHealthInput, PluginListInput};
use serde_json::Value;

use crate::error::DaemonError;
use crate::op_adapter::ok_envelope;
use crate::DispatchContext;

pub(crate) fn op_list(
    input: PluginListInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    Ok(ok_envelope(services.plugin.builtin_plugin_list()))
}

pub(crate) fn op_health(
    input: PluginHealthInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    let output = services
        .plugin
        .builtin_plugin_health(input.layer_stack_root.as_deref())?;
    Ok(ok_envelope(output))
}
