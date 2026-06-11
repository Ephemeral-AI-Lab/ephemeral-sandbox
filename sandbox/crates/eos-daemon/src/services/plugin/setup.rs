//! Plugin setup/config helpers for the daemon facade.

use eos_plugin::PluginManifest;

use crate::error::DaemonError;

use super::service::stop_services_for_layer_stack_root as stop_services_for_layer_stack_root_in_state;
use super::state::{setup_failure_key, PluginRuntime, SetupFailure};

impl PluginRuntime {
    /// PPC socket root for `ParsedEnsure` spec construction, from the typed
    /// runtime config.
    pub(super) fn ppc_socket_root(&self) -> String {
        self.config.ppc_root.to_string_lossy().into_owned()
    }

    pub(super) fn record_setup_failure(
        &self,
        manifest: Option<&PluginManifest>,
        err: &DaemonError,
    ) {
        let Some(manifest) = manifest else {
            return;
        };
        if let Ok(mut state) = self.lock_state() {
            state.setup_failures.insert(
                setup_failure_key(&manifest.plugin_id, &manifest.plugin_digest),
                SetupFailure {
                    plugin: manifest.plugin_id.clone(),
                    digest: manifest.plugin_digest.clone(),
                    error: err.to_string(),
                },
            );
        }
    }

    /// Stop and forget every connected service holding a snapshot on
    /// `layer_stack_root` (the workspace-base reset path).
    pub(crate) fn stop_services_for_layer_stack_root(
        &self,
        layer_stack_root: &str,
    ) -> Result<usize, DaemonError> {
        let mut state = self.lock_state()?;
        Ok(stop_services_for_layer_stack_root_in_state(
            &mut state,
            layer_stack_root,
        ))
    }
}
