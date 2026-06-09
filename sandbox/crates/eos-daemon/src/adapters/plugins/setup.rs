//! Plugin setup/config helpers for the daemon facade.

use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use eos_plugin::host::ensure_args::validate_plugin_caller_fields;
use eos_plugin::host::PackageEnsureReport;
use eos_plugin::{PluginError, PluginManifest};
use serde_json::{json, Value};

use crate::config::PluginRuntimeConfig;
use crate::error::DaemonError;

use super::service::stop_services_for_layer_stack_root as stop_services_for_layer_stack_root_in_state;
use super::state::{lock_state, setup_failure_key};

pub(crate) fn configure_plugin_runtime(config: &PluginRuntimeConfig) {
    let mut guard = plugin_runtime_config_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = config.clone();
}

pub(super) fn plugin_runtime_config() -> PluginRuntimeConfig {
    plugin_runtime_config_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn plugin_runtime_config_cell() -> &'static RwLock<PluginRuntimeConfig> {
    static CONFIG: OnceLock<RwLock<PluginRuntimeConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(default_plugin_runtime_config()))
}

/// PPC socket root for `ParsedEnsure` spec construction. Reads the daemon
/// runtime config global, so it stays daemon-side and is threaded into the
/// host-neutral parser.
pub(super) fn ppc_socket_root(args: &Value) -> String {
    #[cfg(test)]
    {
        if let Some(root) = args.get("ppc_socket_root").and_then(Value::as_str) {
            return root.to_owned();
        }
    }
    let _ = args;
    plugin_runtime_config()
        .ppc_root
        .to_string_lossy()
        .into_owned()
}

fn default_plugin_runtime_config() -> PluginRuntimeConfig {
    PluginRuntimeConfig {
        ppc_root: PathBuf::from("/eos/plugin/ppc"),
        ppc_timeout_ms: 5_000,
        service_probe_timeout_ms: 5_000,
        max_response_bytes: 8 * 1024 * 1024,
    }
}

pub(super) fn ensure_plugin_family_allowed(args: &Value) -> Result<(), DaemonError> {
    validate_plugin_caller_fields(args)?;
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !caller_id.is_empty()
        && crate::adapters::workspace_run::isolated::caller_has_active_handle(caller_id)
    {
        return Err(DaemonError::Plugin(
            PluginError::ForbiddenInIsolatedWorkspace,
        ));
    }
    Ok(())
}

pub(super) fn package_report_value(report: &PackageEnsureReport) -> Value {
    if !report.active {
        return Value::Null;
    }
    json!({
        "needs_upload": report.needs_upload,
        "package_root": report.package_root.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "dependency_root": report.dependency_root.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "package_published": report.package_published,
        "setup_ran": report.setup_ran,
    })
}

pub(super) fn record_setup_failure(manifest: Option<&PluginManifest>, err: &DaemonError) {
    let Some(manifest) = manifest else {
        return;
    };
    if let Ok(mut state) = lock_state() {
        state.setup_failures.insert(
            setup_failure_key(&manifest.plugin_id, &manifest.plugin_digest),
            json!({
                "plugin": manifest.plugin_id,
                "digest": manifest.plugin_digest,
                "error": err.to_string(),
            }),
        );
    }
}

pub(crate) fn stop_services_for_layer_stack_root(
    layer_stack_root: &str,
) -> Result<usize, DaemonError> {
    let mut state = lock_state()?;
    Ok(stop_services_for_layer_stack_root_in_state(
        &mut state,
        layer_stack_root,
    ))
}
