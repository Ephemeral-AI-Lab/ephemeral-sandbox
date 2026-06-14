use operation::plugin::contract::{
    PyrightLspDefinitionInput, PyrightLspDiagnosticsInput, PyrightLspQuerySymbolsInput,
    PyrightLspReferencesInput,
};
use plugin::PluginRuntimeError;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::op_adapter::{ok_envelope, rejected_fault_envelope};
use crate::DispatchContext;

pub(crate) fn op_pyright_lsp_query_symbols(
    input: PyrightLspQuerySymbolsInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    pyright_response(services.plugin.pyright_lsp_query_symbols(&input))
}

pub(crate) fn op_pyright_lsp_definition(
    input: PyrightLspDefinitionInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    pyright_response(services.plugin.pyright_lsp_definition(&input))
}

pub(crate) fn op_pyright_lsp_references(
    input: PyrightLspReferencesInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    pyright_response(services.plugin.pyright_lsp_references(&input))
}

pub(crate) fn op_pyright_lsp_diagnostics(
    input: PyrightLspDiagnosticsInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    services.ensure_plugin_caller_allowed(&input.caller)?;
    pyright_response(services.plugin.pyright_lsp_diagnostics(&input))
}

fn pyright_response(result: Result<Value, PluginRuntimeError>) -> Result<Value, DaemonError> {
    match result {
        Ok(output) => Ok(ok_envelope(output)),
        Err(PluginRuntimeError::PluginDisabled(provider)) => Ok(rejected_fault_envelope(
            "plugin_disabled",
            format!("plugin provider {provider} is disabled"),
            json!({ "provider": provider }),
        )),
        Err(err) => Err(DaemonError::from(err)),
    }
}
