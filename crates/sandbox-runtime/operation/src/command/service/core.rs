use std::sync::{Arc, Mutex, PoisonError};

use sandbox_observability_telemetry::{Observer, SpanRegistry};
use sandbox_runtime_namespace_execution::{
    ExecutionCaps, NamespaceExecutionEngine, NamespaceExecutionId,
};

use crate::command::scratch_route::observed_scratch_route;
use crate::command::terminal_cache::TerminalDrainCache;
use crate::command::{CommandConfig, CommandExecValue};
use crate::namespace_execution::{
    RuntimeNamespaceExecutionSnapshot, WorkspaceCommandReleaseProof, WorkspaceCommandTeardown,
};
use crate::workspace_crate::{
    LegacyExecutionScratchLocator, WorkspaceScratchLocator, WorkspaceSessionId,
    SCRATCH_LAYOUT_VERSION,
};
use crate::workspace_scratch_compat::LegacyScratchReapReport;
use crate::workspace_session::WorkspaceSessionService;

use super::teardown::{
    cancel_and_join_commands, CommandTeardownFailure, CommandTeardownTarget, COMMAND_JOIN_TIMEOUT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandScratchOwnershipSnapshot {
    pub layout_version: u8,
    pub route: &'static str,
    pub active_leases: usize,
    pub retained_terminal_records: usize,
    pub open_transcript_descriptors: usize,
    pub live_bytes: u64,
    pub high_water_bytes: u64,
    pub teardown_join_total: u64,
    pub teardown_deadline_total: u64,
    pub cleanup_state: &'static str,
    pub quiescent: bool,
    pub legacy: LegacyScratchReapReport,
}

pub struct CommandOperationService {
    workspace: Arc<WorkspaceSessionService>,
    config: CommandConfig,
    scratch_locator: WorkspaceScratchLocator,
    legacy_scratch_locator: Option<LegacyExecutionScratchLocator>,
    engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
    exec_spans: Arc<SpanRegistry<NamespaceExecutionId>>,
    obs: Observer,
    legacy_scratch_reap: Mutex<LegacyScratchReapReport>,
    terminal_drains: Arc<TerminalDrainCache>,
}

impl CommandOperationService {
    #[must_use]
    pub fn new(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        obs: Observer,
    ) -> Self {
        let scratch_locator = WorkspaceScratchLocator::new(config.scratch_root.clone())
            .expect("command workspace scratch root must be valid");
        Self::new_with_locator(workspace, config, scratch_locator, obs)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_locator(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        scratch_locator: WorkspaceScratchLocator,
        obs: Observer,
    ) -> Self {
        Self::new_with_locators(workspace, config, scratch_locator, None, obs)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_locators(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        scratch_locator: WorkspaceScratchLocator,
        legacy_scratch_locator: Option<LegacyExecutionScratchLocator>,
        obs: Observer,
    ) -> Self {
        let exec_spans = Arc::new(SpanRegistry::new(obs.clone()));
        let engine = Arc::new(NamespaceExecutionEngine::new(
            exec_spans.clone(),
            ExecutionCaps {
                max_active: config.max_active,
                setup_timeout_s: config.setup_timeout_s,
                stdin_write_deadline: std::time::Duration::from_secs_f64(
                    config.execution.stdin_write_deadline_s,
                ),
                max_terminal_entries: config.execution.max_terminal_entries,
                max_transcript_window_bytes: config.execution.max_transcript_window_bytes,
                max_runner_result_bytes: config.execution.max_runner_result_bytes,
                command_security_profile: config.execution.command_security_profile,
            },
        ));
        Self::with_engine_and_locators(
            workspace,
            config,
            scratch_locator,
            legacy_scratch_locator,
            engine,
            exec_spans,
            obs,
        )
    }

    /// Build a command service over a caller-supplied engine and the exec span
    /// registry wired into it. The same `exec_spans` must back both the engine's
    /// terminal hook and this service's launch path, so a parked span always has
    /// a recorder. The test harness wires the engine to a local fake launcher;
    /// production goes through `new`.
    #[doc(hidden)]
    #[must_use]
    pub fn with_engine(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
        exec_spans: Arc<SpanRegistry<NamespaceExecutionId>>,
        obs: Observer,
    ) -> Self {
        let scratch_locator = WorkspaceScratchLocator::new(config.scratch_root.clone())
            .expect("command workspace scratch root must be valid");
        Self::with_engine_and_locator(workspace, config, scratch_locator, engine, exec_spans, obs)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_engine_and_locator(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        scratch_locator: WorkspaceScratchLocator,
        engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
        exec_spans: Arc<SpanRegistry<NamespaceExecutionId>>,
        obs: Observer,
    ) -> Self {
        Self::with_engine_and_locators(
            workspace,
            config,
            scratch_locator,
            None,
            engine,
            exec_spans,
            obs,
        )
    }

    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_engine_and_locators(
        workspace: Arc<WorkspaceSessionService>,
        config: CommandConfig,
        scratch_locator: WorkspaceScratchLocator,
        legacy_scratch_locator: Option<LegacyExecutionScratchLocator>,
        engine: Arc<NamespaceExecutionEngine<CommandExecValue>>,
        exec_spans: Arc<SpanRegistry<NamespaceExecutionId>>,
        obs: Observer,
    ) -> Self {
        let teardown: Arc<dyn WorkspaceCommandTeardown> = engine.clone();
        workspace.register_command_teardown(&teardown);
        let terminal_drains = Arc::new(TerminalDrainCache::new(
            config.execution.max_terminal_entries,
        ));
        Self {
            workspace,
            config,
            scratch_locator,
            legacy_scratch_locator,
            engine,
            exec_spans,
            obs,
            legacy_scratch_reap: Mutex::new(LegacyScratchReapReport::default()),
            terminal_drains,
        }
    }

    #[must_use]
    pub fn active_namespace_executions(&self) -> Vec<RuntimeNamespaceExecutionSnapshot> {
        let mut snapshots = self.engine.live_values(|command| {
            Some(RuntimeNamespaceExecutionSnapshot {
                namespace_execution_id: command.exec.id().clone(),
                workspace_session_id: command.workspace_session_id.clone(),
                operation_name: command.operation_name.to_owned(),
                command: Some(command.command.clone()),
            })
        });
        snapshots.sort_by(|left, right| {
            left.namespace_execution_id
                .cmp(&right.namespace_execution_id)
        });
        snapshots
    }

    #[must_use]
    pub fn active_namespace_execution_count(&self) -> usize {
        self.engine.active_count()
    }

    #[must_use]
    pub fn config(&self) -> &CommandConfig {
        &self.config
    }

    pub(super) fn scratch_locator(&self) -> &WorkspaceScratchLocator {
        &self.scratch_locator
    }

    pub(super) fn legacy_scratch_locator(&self) -> Option<&LegacyExecutionScratchLocator> {
        self.legacy_scratch_locator.as_ref()
    }

    pub(super) fn terminal_drains(&self) -> &Arc<TerminalDrainCache> {
        &self.terminal_drains
    }

    pub(crate) fn record_legacy_scratch_reap(&self, report: LegacyScratchReapReport) {
        *self
            .legacy_scratch_reap
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = report;
    }

    #[must_use]
    pub(crate) fn legacy_scratch_reap_report(&self) -> LegacyScratchReapReport {
        self.legacy_scratch_reap
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub(crate) fn scratch_ownership_snapshot(&self) -> CommandScratchOwnershipSnapshot {
        let values = self
            .engine
            .value_metrics(CommandExecValue::transcript_bytes);
        let routes = self
            .engine
            .retained_values(|command| Some(command.scratch_route()));
        let route = observed_scratch_route(&routes);
        let workers = self.engine.background_worker_snapshot();
        let (teardown_join_total, teardown_deadline_total) = self.engine.teardown_totals();
        let high_water_bytes = self
            .engine
            .observe_scratch_bytes_high_water(values.total_value_units);
        let quiescent = values.active_values == 0
            && values.terminal_values == 0
            && workers.active_pty_readers == 0
            && values.total_value_units == 0;
        let cleanup_state = if quiescent {
            "quiescent"
        } else if values.active_values > 0 && teardown_deadline_total > 0 {
            "recovery_pending"
        } else if values.active_values > 0 {
            "active"
        } else {
            "terminal_retained"
        };
        CommandScratchOwnershipSnapshot {
            layout_version: SCRATCH_LAYOUT_VERSION,
            route,
            active_leases: values.active_values,
            retained_terminal_records: values.terminal_values,
            open_transcript_descriptors: workers.active_pty_readers,
            live_bytes: values.total_value_units,
            high_water_bytes,
            teardown_join_total,
            teardown_deadline_total,
            cleanup_state,
            quiescent,
            legacy: self.legacy_scratch_reap_report(),
        }
    }

    #[must_use]
    pub(crate) fn engine(&self) -> &Arc<NamespaceExecutionEngine<CommandExecValue>> {
        &self.engine
    }

    #[must_use]
    pub(super) fn obs(&self) -> &Observer {
        &self.obs
    }

    #[must_use]
    pub(super) fn exec_spans(&self) -> &Arc<SpanRegistry<NamespaceExecutionId>> {
        &self.exec_spans
    }

    #[must_use]
    pub(super) fn workspace_handle(&self) -> &Arc<WorkspaceSessionService> {
        &self.workspace
    }

    /// Stop admitting namespace commands and join the shared completion reaper.
    pub fn shutdown_and_join(&self) -> Result<(), String> {
        self.engine
            .shutdown_and_join()
            .map_err(|error| error.to_string())
    }
}

impl WorkspaceCommandTeardown for NamespaceExecutionEngine<CommandExecValue> {
    fn cancel_and_join(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_ids: &[NamespaceExecutionId],
    ) -> Result<(), String> {
        let result = cancel_and_join_commands(
            workspace_session_id,
            command_ids,
            COMMAND_JOIN_TIMEOUT,
            |command_id| {
                self.with_value(command_id, |command| CommandTeardownTarget {
                    owner: command.workspace_session_id.clone(),
                    cancel: command.exec.cancel_handle(),
                    completion: command.exec.completion(),
                })
            },
        );
        let deadline_count = result.as_ref().err().map_or(0, |error| {
            error
                .failures
                .iter()
                .filter(|failure| matches!(failure, CommandTeardownFailure::JoinTimedOut { .. }))
                .count()
        });
        self.record_teardown(command_ids.len(), deadline_count);
        result.map_err(|error| error.to_string())
    }

    fn release_for_destroy(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<WorkspaceCommandReleaseProof, String> {
        let released = self.visit_terminal_values(|command| {
            if command.workspace_session_id != *workspace_session_id {
                return Ok(false);
            }
            command.release_scratch()
        })?;
        let active_owners = self.live_values(|command| {
            (command.workspace_session_id == *workspace_session_id).then_some(())
        });
        if active_owners.is_empty() {
            let removed = self.remove_terminal_values(|command| {
                command.workspace_session_id == *workspace_session_id
            });
            debug_assert!(removed >= released);
            Ok(WorkspaceCommandReleaseProof::new(
                workspace_session_id.clone(),
                removed,
            ))
        } else {
            Err(format!(
                "{} active command scratch owner(s) remain for workspace {}",
                active_owners.len(),
                workspace_session_id.0
            ))
        }
    }
}
