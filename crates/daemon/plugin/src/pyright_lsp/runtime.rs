use std::path::PathBuf;
use std::time::Duration;

use config::configs::daemon::{PluginRuntimeConfig, PyrightLspConfig, PYRIGHT_LSP_PLUGIN_ID};
use layerstack::LayerStack;
use serde_json::{json, Value};

use crate::PluginRuntimeError;

use super::command::resolve_pyright_command;
use super::process::LspProcess;
use super::projection::{manifest_key, project_snapshot, release_snapshot};

pub(crate) struct PyrightLspRuntime {
    process: Option<LspProcess>,
    active_manifest_key: Option<String>,
    pub(super) projection_root: PathBuf,
    initialize_result: Option<Value>,
    resolved_command: Vec<String>,
    last_init_error: Option<String>,
    last_refresh_error: Option<String>,
    pub(super) last_analysis_error: Option<String>,
    last_process_exit_error: Option<String>,
}

impl PyrightLspRuntime {
    pub(crate) fn new(config: &PyrightLspConfig) -> Self {
        Self {
            process: None,
            active_manifest_key: None,
            projection_root: config.workspace_root.clone(),
            initialize_result: None,
            resolved_command: Vec::new(),
            last_init_error: None,
            last_refresh_error: None,
            last_analysis_error: None,
            last_process_exit_error: None,
        }
    }

    pub(super) fn ensure_ready(
        &mut self,
        config: &PluginRuntimeConfig,
        layer_stack_root: &str,
        target_file: Option<&str>,
    ) -> Result<ReadyPyright<'_>, PluginRuntimeError> {
        let projection = self.ensure_projection_current(&config.pyright_lsp, layer_stack_root)?;
        let process_missing = self.process.is_none();
        if process_missing {
            self.start_process(config)?;
        }
        let Some(process) = self.process.as_mut() else {
            return Err(PluginRuntimeError::PyrightLsp(
                "pyright_lsp process was not started".to_owned(),
            ));
        };
        if let Some(file_path) = target_file {
            process.open_document(
                &projection.projection_root,
                file_path,
                projection.manifest_version,
            )?;
        }
        Ok(ReadyPyright {
            process,
            manifest_key: projection.manifest_key,
            projection_root: projection.projection_root,
        })
    }

    fn ensure_projection_current(
        &mut self,
        config: &PyrightLspConfig,
        layer_stack_root: &str,
    ) -> Result<ProjectionState, PluginRuntimeError> {
        let stack_root = PathBuf::from(layer_stack_root);
        let stack = LayerStack::open(stack_root.clone())?;
        let lease = stack.acquire_snapshot("pyright_lsp:projection")?;
        let manifest_key = manifest_key(lease.manifest_version, &lease.root_hash);
        let manifest_version = lease.manifest_version;
        let projection_root = config.workspace_root.clone();
        if self.active_manifest_key.as_deref() == Some(&manifest_key)
            && projection_root == self.projection_root
        {
            release_snapshot(&stack_root, &lease.lease_id);
            return Ok(ProjectionState {
                manifest_key,
                manifest_version,
                projection_root,
            });
        }

        self.stop_process();
        let project_result = project_snapshot(&stack_root, &projection_root, &lease.manifest);
        release_snapshot(&stack_root, &lease.lease_id);
        if let Err(err) = project_result {
            let message = err.to_string();
            self.last_refresh_error = Some(message.clone());
            return Err(PluginRuntimeError::PyrightLsp(message));
        }
        self.projection_root = projection_root.clone();
        self.active_manifest_key = Some(manifest_key.clone());
        self.last_refresh_error = None;
        Ok(ProjectionState {
            manifest_key,
            manifest_version,
            projection_root,
        })
    }

    fn start_process(&mut self, config: &PluginRuntimeConfig) -> Result<(), PluginRuntimeError> {
        let command = resolve_pyright_command(&config.pyright_lsp)?;
        let timeout = Duration::from_millis(config.pyright_lsp.analysis_timeout_ms)
            .max(Duration::from_secs(60));
        let mut process = LspProcess::start(
            command.clone(),
            &self.projection_root,
            config.max_response_bytes,
        )
        .map_err(|err| {
            self.last_init_error = Some(err.clone());
            PluginRuntimeError::PyrightLsp(err)
        })?;
        let initialize_result =
            process
                .initialize(&self.projection_root, timeout)
                .map_err(|err| {
                    self.last_init_error = Some(err.clone());
                    PluginRuntimeError::PyrightLsp(err)
                })?;
        self.resolved_command = command;
        self.initialize_result = Some(initialize_result);
        self.last_init_error = None;
        self.process = Some(process);
        Ok(())
    }

    fn stop_process(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.teardown();
        }
        self.initialize_result = None;
        self.resolved_command.clear();
    }

    pub(super) fn health_value(&mut self, config: &PluginRuntimeConfig, enabled: bool) -> Value {
        let running = self.process.as_mut().is_some_and(LspProcess::is_running);
        let pid = self.process.as_ref().map(LspProcess::pid);
        let capabilities = self
            .initialize_result
            .as_ref()
            .and_then(|result| result.get("capabilities"))
            .cloned()
            .unwrap_or(Value::Null);
        let server_info = self
            .initialize_result
            .as_ref()
            .and_then(|result| result.get("serverInfo"))
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "provider": PYRIGHT_LSP_PLUGIN_ID,
            "enabled": enabled,
            "running": running,
            "process_id": pid,
            "pid": pid,
            "node_path": config.pyright_lsp.node_path,
            "pyright_langserver_path": config.pyright_lsp.pyright_langserver_path,
            "resolved_command": self.resolved_command,
            "initialize_success": self.initialize_result.is_some(),
            "capabilities": capabilities,
            "server_info": server_info,
            "active_manifest_key": self.active_manifest_key,
            "projection_root": self.projection_root,
            "last_init_error": self.last_init_error,
            "last_refresh_error": self.last_refresh_error,
            "last_analysis_error": self.last_analysis_error,
            "last_process_exit_error": self.last_process_exit_error,
        })
    }
}

pub(super) struct ReadyPyright<'a> {
    pub(super) process: &'a mut LspProcess,
    pub(super) manifest_key: String,
    pub(super) projection_root: PathBuf,
}

struct ProjectionState {
    manifest_key: String,
    manifest_version: i64,
    projection_root: PathBuf,
}
