use std::time::Duration;

use config::configs::daemon::PYRIGHT_LSP_PLUGIN_ID;
use operation::plugin::contract::{
    PyrightLspDefinitionInput, PyrightLspDiagnosticsInput, PyrightLspQuerySymbolsInput,
    PyrightLspReferencesInput,
};
use serde_json::{json, Value};

use crate::{PluginRuntime, PluginRuntimeError};

use super::responses::{base_pyright_response, pyright_timeout_response};

impl PluginRuntime {
    #[must_use]
    pub fn builtin_plugin_list(&self) -> Value {
        json!({
            "success": true,
            "providers": [{
                "provider": PYRIGHT_LSP_PLUGIN_ID,
                "enabled": self.config.pyright_lsp_enabled(),
                "state": if self.config.pyright_lsp_enabled() { "enabled" } else { "disabled" },
            }],
        })
    }

    pub fn builtin_plugin_health(
        &self,
        layer_stack_root: Option<&str>,
    ) -> Result<Value, PluginRuntimeError> {
        let enabled = self.config.pyright_lsp_enabled();
        if enabled {
            let root = layer_stack_root.ok_or_else(|| {
                PluginRuntimeError::InvalidRequest(
                    "layer_stack_root is required for sandbox.plugin.health".to_owned(),
                )
            })?;
            let mut runtime = self.lock_pyright_lsp()?;
            runtime.ensure_ready(&self.config, root, None)?;
            Ok(json!({
                "success": true,
                "providers": [runtime.health_value(&self.config, true)],
            }))
        } else {
            let mut runtime = self.lock_pyright_lsp()?;
            Ok(json!({
                "success": true,
                "providers": [runtime.health_value(&self.config, false)],
            }))
        }
    }

    pub fn pyright_lsp_query_symbols(
        &self,
        input: &PyrightLspQuerySymbolsInput,
    ) -> Result<Value, PluginRuntimeError> {
        self.ensure_pyright_enabled()?;
        let mut runtime = self.lock_pyright_lsp()?;
        let ready = runtime.ensure_ready(
            &self.config,
            &input.layer_stack_root,
            Some(&input.file_path),
        )?;
        let symbols = ready.process.document_symbols(
            &ready.projection_root,
            &input.file_path,
            input.query.as_deref(),
        )?;
        Ok(base_pyright_response(
            &ready.manifest_key,
            json!({ "symbols": symbols }),
        ))
    }

    pub fn pyright_lsp_definition(
        &self,
        input: &PyrightLspDefinitionInput,
    ) -> Result<Value, PluginRuntimeError> {
        self.ensure_pyright_enabled()?;
        let mut runtime = self.lock_pyright_lsp()?;
        let ready = runtime.ensure_ready(
            &self.config,
            &input.layer_stack_root,
            Some(&input.file_path),
        )?;
        let locations = ready.process.definition(
            &ready.projection_root,
            &input.file_path,
            input.position.line,
            input.position.character,
        )?;
        Ok(base_pyright_response(
            &ready.manifest_key,
            json!({ "locations": locations }),
        ))
    }

    pub fn pyright_lsp_references(
        &self,
        input: &PyrightLspReferencesInput,
    ) -> Result<Value, PluginRuntimeError> {
        self.ensure_pyright_enabled()?;
        let mut runtime = self.lock_pyright_lsp()?;
        let ready = runtime.ensure_ready(
            &self.config,
            &input.layer_stack_root,
            Some(&input.file_path),
        )?;
        let locations = ready.process.references(
            &ready.projection_root,
            &input.file_path,
            input.position.line,
            input.position.character,
            input.include_declaration,
        )?;
        Ok(base_pyright_response(
            &ready.manifest_key,
            json!({ "locations": locations }),
        ))
    }

    pub fn pyright_lsp_diagnostics(
        &self,
        input: &PyrightLspDiagnosticsInput,
    ) -> Result<Value, PluginRuntimeError> {
        self.ensure_pyright_enabled()?;
        let timeout = Duration::from_millis(self.config.pyright_lsp.analysis_timeout_ms);
        let mut runtime = self.lock_pyright_lsp()?;
        let (manifest_key, diagnostics_result) = {
            let ready = runtime.ensure_ready(
                &self.config,
                &input.layer_stack_root,
                Some(&input.file_path),
            )?;
            (
                ready.manifest_key.clone(),
                ready
                    .process
                    .diagnostics(&ready.projection_root, &input.file_path, timeout),
            )
        };
        let diagnostics = match diagnostics_result {
            Ok(diagnostics) => {
                runtime.last_analysis_error = None;
                base_pyright_response(&manifest_key, json!({ "diagnostics": diagnostics }))
            }
            Err(err) if err.starts_with("timed out waiting for diagnostics") => {
                runtime.last_analysis_error = Some(err.clone());
                pyright_timeout_response(&manifest_key, err)
            }
            Err(err) => {
                runtime.last_analysis_error = Some(err.clone());
                return Err(PluginRuntimeError::PyrightLsp(err));
            }
        };
        Ok(diagnostics)
    }

    fn ensure_pyright_enabled(&self) -> Result<(), PluginRuntimeError> {
        if self.config.pyright_lsp_enabled() {
            Ok(())
        } else {
            Err(PluginRuntimeError::PluginDisabled(
                PYRIGHT_LSP_PLUGIN_ID.to_owned(),
            ))
        }
    }
}
