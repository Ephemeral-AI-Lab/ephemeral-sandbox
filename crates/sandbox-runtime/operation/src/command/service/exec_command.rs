use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::Instant;

use sandbox_observability_telemetry::record::names;
use sandbox_runtime_namespace_execution::{
    CommandSecurityProfile, NamespaceExecutionError, NamespaceExecutionId, NamespaceTarget,
};
use serde_json::json;

use crate::command::service::exec::ExecCommand;
use crate::command::service::CommandOperationService;
use crate::command::{
    CommandExecValue, CommandOutput, CommandServiceError, CommandTerminalResult, ExecCommandInput,
};
use crate::workspace_crate::{
    DestroyWorkspaceRequest, ExecutionScratchRoute, NetworkProfile, WorkspaceEntry,
};
use crate::workspace_session::{
    AdmittedCommand, CreateSessionRequest, FinalizePolicy, WorkspaceSessionError,
    WorkspaceSessionHandler,
};

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        self.obs().scope(names::COMMAND_EXEC, |span| {
            if input.cmd.trim().is_empty() {
                return Err(CommandServiceError::InvalidCommand {
                    message: "cmd must be non-empty".to_owned(),
                });
            }
            let command_security_profile = selected_command_security_profile(
                self.config().execution.command_security_profile,
                &input.cmd,
            );
            span.attr(
                "command_security_profile",
                match command_security_profile {
                    CommandSecurityProfile::Standard => "standard",
                    CommandSecurityProfile::MplaBenchmarkQualification => {
                        "mpla_benchmark_qualification"
                    }
                },
            );
            span.attr("session_created", input.workspace_session_id.is_none());
            let mut implicit_handler: Option<WorkspaceSessionHandler> = None;
            let workspace_session_id = match input.workspace_session_id.clone() {
                Some(workspace_session_id) => workspace_session_id,
                None => {
                    let handler =
                        self.workspace_handle()
                            .create_workspace_session(CreateSessionRequest {
                                network: NetworkProfile::Shared,
                                finalize_policy: FinalizePolicy::PublishThenDestroy,
                            })?;
                    let workspace_session_id = handler.workspace_session_id.clone();
                    implicit_handler = Some(handler);
                    workspace_session_id
                }
            };

            let id = self.engine().allocate_id();
            let gate = self.workspace_handle().session_gate(&workspace_session_id);
            let admitted: AdmittedCommand;
            let admission_guard = gate.lock().unwrap_or_else(PoisonError::into_inner);
            admitted = match self.workspace_handle().admit_command_locked(
                &gate,
                &admission_guard,
                &workspace_session_id,
                id.clone(),
            ) {
                Ok(admitted) => admitted,
                Err(error) => {
                    return Err(self.cleanup_implicit_session(implicit_handler, error.into()));
                }
            };
            span.attr("finalize_policy", admitted.finalize_policy.as_str());

            span.attr("scratch_layout_version", 2_u64);
            span.attr(
                "scratch_route",
                admitted.handler.execution_scratch_route.as_str(),
            );
            span.attr(
                "workspace_session_hash",
                stable_identifier_hash(&workspace_session_id.0),
            );
            span.attr("namespace_execution_hash", stable_identifier_hash(&id.0));

            let (entry, execution_scratch) =
                match workspace_entry(&admitted.handler).and_then(|entry| {
                    self.prepare_execution_scratch(
                        admitted.handler.execution_scratch_route,
                        &workspace_session_id,
                        &id,
                    )
                    .map(|scratch| (entry, scratch))
                }) {
                    Ok(pair) => pair,
                    Err(error) => {
                        self.complete_admitted(&admitted, admission_guard);
                        return Err(error);
                    }
                };
            let transcript_path = execution_scratch.transcript_path().to_path_buf();

            let started_at = Instant::now();
            let exec_command = ExecCommand {
                command: input.cmd.clone(),
                timeout_seconds: input.timeout_ms.map(|ms| ms as f64 / 1000.0),
                transcript_path: transcript_path.clone(),
                started_at,
            };
            let on_complete = {
                let engine = Arc::clone(self.engine());
                let terminal_drains = Arc::clone(self.terminal_drains());
                let command_session_id = id.clone();
                let retain_terminal_drain =
                    admitted.finalize_policy == FinalizePolicy::PublishThenDestroy;
                let token_slot = Arc::clone(&admitted.token_slot);
                let obs = self.obs().clone();
                let ctx = self.obs().context();
                move |result: &Result<CommandTerminalResult, NamespaceExecutionError>| {
                    if retain_terminal_drain {
                        if let Some(record) = engine.with_value(&command_session_id, |command| {
                            command.terminal_drain_record(result.clone())
                        }) {
                            terminal_drains.insert(command_session_id, record);
                        }
                    }
                    let token = token_slot
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take();
                    if let Some(token) = token {
                        obs.with_context(ctx, || drop(token));
                    }
                }
            };
            let target = NamespaceTarget::from(entry);

            let cgroup_procs_path = admitted
                .handler
                .cgroup_path
                .as_ref()
                .map(|cgroup| cgroup.join("cgroup.procs"));
            let observability_log_path = self.obs().log_path();
            let attach_engine = Arc::clone(self.engine());
            let attach_id = id.clone();
            let attach_workspace_session_id = admitted.handler.workspace_session_id.clone();
            let attach_finalize_outcome = Arc::clone(&admitted.finalize_outcome);
            let attach_command = input.cmd.clone();
            let max_transcript_window_bytes = self.config().execution.max_transcript_window_bytes;
            let exec = self.exec_spans().launch(
                id.clone(),
                self.obs().context(),
                names::NAMESPACE_EXEC_RUN_SHELL,
                |child_ctx| {
                    let trace_handoff = child_ctx.zip(observability_log_path);
                    self.engine()
                        .run_shell_interactive_attached_with_security_profile(
                            exec_command,
                            target,
                            id.clone(),
                            move |exec| {
                                attach_engine.attach(
                                    &attach_id,
                                    CommandExecValue::new(
                                        exec,
                                        execution_scratch,
                                        attach_workspace_session_id,
                                        started_at,
                                        "exec_command",
                                        attach_command,
                                        attach_finalize_outcome,
                                        max_transcript_window_bytes,
                                    ),
                                );
                            },
                            on_complete,
                            cgroup_procs_path,
                            trace_handoff,
                            command_security_profile,
                        )
                },
            );
            match exec {
                Ok(_) => {}
                Err(error) => {
                    let error = match error {
                        NamespaceExecutionError::Admission { max_active } => {
                            CommandServiceError::CommandAdmissionOverloaded {
                                max_active_commands: max_active,
                            }
                        }
                        NamespaceExecutionError::Duplicate { execution_id } => {
                            CommandServiceError::CommandAlreadyExists {
                                command_session_id: NamespaceExecutionId(execution_id),
                            }
                        }
                        error => CommandServiceError::CommandIo {
                            command_session_id: id.clone(),
                            error: error.to_string(),
                        },
                    };
                    self.complete_admitted(&admitted, admission_guard);
                    return Err(error);
                }
            }
            drop(admission_guard);

            self.wait_for_command_yield(id, input.yield_time_ms.unwrap_or(1000), false)
        })
    }

    /// Take the token from the slot and complete it under the held admission
    /// guard (§2.3 failure path): the ledger entry is removed and the finalize
    /// policy runs without re-locking the gate.
    fn complete_admitted(&self, admitted: &AdmittedCommand, admission_guard: MutexGuard<'_, ()>) {
        let token = admitted
            .token_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(token) = token {
            self.workspace_handle()
                .complete_admitted_locked(token, admission_guard);
        }
    }

    /// Pre-admission failure cleanup: destroy the implicitly created session
    /// directly. If that destroy itself fails, the original command error
    /// still surfaces and the destroy failure is recorded as a
    /// `workspace_session.cleanup_failed` event (§2.4 / F10).
    fn cleanup_implicit_session(
        &self,
        implicit_handler: Option<WorkspaceSessionHandler>,
        error: CommandServiceError,
    ) -> CommandServiceError {
        let Some(handler) = implicit_handler else {
            return error;
        };
        let workspace_session_id = handler.workspace_session_id.clone();
        if let Err(cleanup_error) = self
            .workspace_handle()
            .destroy_session(handler, DestroyWorkspaceRequest::default())
        {
            if !matches!(cleanup_error, WorkspaceSessionError::NotFound { .. }) {
                self.obs().event(
                    names::WORKSPACE_SESSION_CLEANUP_FAILED,
                    json!({
                        "workspace_session_id": workspace_session_id.0,
                        "error": cleanup_error.to_string(),
                    }),
                );
            }
        }
        error
    }

    fn prepare_execution_scratch(
        &self,
        route: ExecutionScratchRoute,
        workspace_session_id: &crate::workspace_crate::WorkspaceSessionId,
        id: &NamespaceExecutionId,
    ) -> Result<crate::workspace_crate::ExecutionScratchLease, CommandServiceError> {
        let result = match route {
            ExecutionScratchRoute::WorkspaceScoped => self
                .scratch_locator()
                .allocate_execution(workspace_session_id, id),
            ExecutionScratchRoute::LegacyCompat => {
                let Some(locator) = self.legacy_scratch_locator() else {
                    return Err(CommandServiceError::CommandIo {
                        command_session_id: id.clone(),
                        error: "legacy scratch compatibility adapter is disabled".to_owned(),
                    });
                };
                locator.allocate_execution(id)
            }
        };
        result.map_err(|error| CommandServiceError::CommandIo {
            command_session_id: id.clone(),
            error: error.to_string(),
        })
    }
}

fn selected_command_security_profile(
    configured: CommandSecurityProfile,
    command: &str,
) -> CommandSecurityProfile {
    if configured == CommandSecurityProfile::MplaBenchmarkQualification
        && is_frozen_mpla_benchmark_command(command)
    {
        CommandSecurityProfile::MplaBenchmarkQualification
    } else {
        CommandSecurityProfile::Standard
    }
}

fn is_frozen_mpla_benchmark_command(command: &str) -> bool {
    const PROGRAM: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1";
    const AUTHORITY_ROOT: &str = "/eos/workspace/mpla-poc/authority/";
    const SPEED_ROOT: &str = "/eos/workspace/mpla-poc/speed/";
    const ORACLE: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle";
    const EXPORTER: &str =
        "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-catalog-export";
    const CATALOG: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/product-catalog.json";
    const LEDGER: &str = "/eos/workspace/samples.jsonl";

    let fields = command.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.iter().any(|field| {
        field.is_empty()
            || !field.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
    }) || fields.first().copied() != Some(PROGRAM)
    {
        return false;
    }

    match fields.as_slice() {
        [_, "authority-probe", "--probe-root", probe_root] => {
            is_direct_safe_child(probe_root, AUTHORITY_ROOT)
        }
        [_, "measure", "--run-id", run_id, "--run-root", run_root, "--oracle", ORACLE, "--catalog-exporter", EXPORTER, "--catalog", CATALOG, "--build-commit", build_commit, "--samples-ledger", LEDGER] => {
            is_safe_identifier(run_id)
                && *run_root == format!("{SPEED_ROOT}{run_id}")
                && build_commit.len() == 40
                && build_commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        _ => false,
    }
}

fn is_direct_safe_child(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|name| is_safe_identifier(name) && !name.contains('/'))
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn stable_identifier_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn workspace_entry(
    handler: &WorkspaceSessionHandler,
) -> Result<WorkspaceEntry, CommandServiceError> {
    handler
        .handle
        .entry()
        .map_err(|error| CommandServiceError::InvalidCommand {
            message: error.to_string(),
        })
}

#[cfg(test)]
mod benchmark_security_profile_tests {
    use super::*;

    const QUALIFICATION: CommandSecurityProfile =
        CommandSecurityProfile::MplaBenchmarkQualification;

    #[test]
    fn only_frozen_benchmark_commands_receive_the_qualification_profile() {
        let authority = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/run-1";
        let measurement = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 measure --run-id run-1 --run-root /eos/workspace/mpla-poc/speed/run-1 --oracle /eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle --catalog-exporter /eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-catalog-export --catalog /eos/layer-stack/base/B000001-base/_campaign-tools/product-catalog.json --build-commit 0123456789abcdef0123456789abcdef01234567 --samples-ledger /eos/workspace/samples.jsonl";
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, authority),
            QUALIFICATION
        );
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, measurement),
            QUALIFICATION
        );
        assert_eq!(
            selected_command_security_profile(
                QUALIFICATION,
                "mount -t tmpfs none .mpla-ordinary-mount-probe"
            ),
            CommandSecurityProfile::Standard
        );
    }

    #[test]
    fn shell_suffixes_and_contract_drift_are_not_privileged() {
        let suffix = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/run-1 ; mount -t tmpfs none /mnt";
        let traversal = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/../escape";
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, suffix),
            CommandSecurityProfile::Standard
        );
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, traversal),
            CommandSecurityProfile::Standard
        );
    }
}
