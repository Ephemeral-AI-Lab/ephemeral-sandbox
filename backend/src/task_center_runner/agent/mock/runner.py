"""MockSquadRunner — deterministic mock agent execution for live e2e scenarios.

The runner dispatches on ``agent_def.agent_kind`` (planner/executor/verifier/evaluator)
plus ``agent_def.name == "entry_executor"`` and calls **real** submission tools
through ``execute_tool_once``.
"""

from __future__ import annotations

import dataclasses
import json
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api
from agents import AgentDefinition
from engine.api import EphemeralRunResult
from message.messages import ConversationMessage, ToolUseBlock
from message.stream_events import (
    AssistantMessageComplete,
    AssistantTextDelta,
    StreamEvent,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from providers.types import UsageSnapshot
from sandbox.api import (
    EditFileRequest,
    SandboxCaller,
    SearchReplaceEdit,
)
from task_center.trial.state import Trial as Attempt
from task_center.iteration.state import Iteration as Episode
from tools._framework.core.base import BaseTool
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools._framework.execution.tool_call import execute_tool_once
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.shell import shell as shell_tool
from tools.sandbox.write_file import write_file as write_file_tool
from tools.submission.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.executor import (
    submit_execution_failure,
    submit_execution_success,
)
from tools.submission.executor.submit_execution_handoff import (
    submit_execution_handoff,
)
from tools.submission.verifier import (
    submit_verification_failure,
    submit_verification_success,
)
from tools.submission.planner import (
    submit_full_plan,
    submit_partial_plan,
)

from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.legacy import LegacySandboxAuditSink
from task_center_runner.audit.node_id import NodeId
from task_center_runner.scenarios.base import (
    Scenario,
    ScenarioContext,
)
from task_center_runner.hooks.registry import MutableMockState
from task_center_runner.agent.mock.prompt_inspector import (
    LaunchRecord,
    PromptInspection,
    ToolCallRecord,
)
from task_center_runner.agent.mock.sandbox_probe import SandboxCheck
from task_center_runner.agent.mock.full_stack_tool_scripts import (
    final_reconciliation_script as full_stack_final_reconciliation_script,
    inspect_full_user_input_script,
    layerstack_squash_lease_script,
    lsp_refresh_semantics_script,
    occ_conflict_matrix_script,
    overlay_edge_matrix_script,
    recursive_oversized_matrix_script,
    verifier_checkpoint_script as full_stack_verifier_checkpoint_script,
)
from task_center_runner.agent.mock.capacity_actions import (
    full_system_capacity_metrics_script,
)
from task_center_runner.agent.mock.tool_scripts import (
    PreparedToolScriptEngine,
    execute_package_script,
    final_reconciliation_script,
    inspect_user_input_script,
    recursive_step_script,
    verifier_checkpoint_script,
)

_PLANNER_EVENT_BY_TOOL: dict[str, EventType] = {
    submit_full_plan.name: EventType.PLANNER_FULL_PLAN,
    submit_partial_plan.name: EventType.PLANNER_PARTIAL_PLAN,
}

_EVALUATOR_EVENT_BY_TOOL: dict[str, EventType] = {
    submit_evaluation_success.name: EventType.EVALUATOR_SUCCESS,
    submit_evaluation_failure.name: EventType.EVALUATOR_FAILURE,
}

_VERIFIER_EVENT_BY_TOOL: dict[str, EventType] = {
    submit_verification_success.name: EventType.VERIFIER_SUCCESS,
    submit_verification_failure.name: EventType.VERIFIER_FAILURE,
}


async def _noop_emit(_event: Any) -> None:
    return None


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]


class MockSquadRunner:
    """Deterministic agent execution handlers that call real tools."""

    def __init__(
        self,
        *,
        repo_dir: str,
        bus: AuditEventBus | None = None,
        task_center_run_id: str = "",
        scenario: Scenario | None = None,
        mutable_state: MutableMockState | None = None,
        audit_recorder: Any | None = None,
    ) -> None:
        # Late import to keep the package-level import graph DAG-shaped:
        # task_center_runner.scenarios re-exports CorrectnessTesting, and importing it
        # eagerly here would create a runner ↔ scenarios cycle.
        from task_center_runner.scenarios.correctness_testing import (
            CorrectnessTesting,
        )

        self._repo_dir = repo_dir
        self._bus = bus
        self._sandbox_audit_sink = (
            LegacySandboxAuditSink(bus) if bus is not None else None
        )
        self._task_center_run_id = task_center_run_id
        self._scenario: Scenario = scenario or CorrectnessTesting()
        self._mutable_state = mutable_state
        self._audit_recorder = audit_recorder
        self._script_engine = PreparedToolScriptEngine(self._call_tool)

    def bind_audit_recorder(self, audit_recorder: Any | None) -> None:
        self._audit_recorder = audit_recorder

    async def __call__(
        self,
        config: Any,
        prompt: str,
        *,
        agent_def: AgentDefinition | None = None,
        sandbox_id: str | None = None,
        extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
        **kwargs: Any,
    ) -> EphemeralRunResult:
        if agent_def is None:
            raise RuntimeError("MockSquadRunner requires agent_def.")
        on_event = kwargs.get("on_event")

        async def emit(event: StreamEvent) -> None:
            if callable(on_event):
                await on_event(event)

        metadata = self._metadata_for(
            config=config,
            agent_def=agent_def,
            sandbox_id=sandbox_id,
            extra_tool_metadata=extra_tool_metadata,
        )
        task_id = str(metadata.get("task_center_task_id") or "")
        attempt_id = str(metadata.get("task_center_attempt_id") or "") or None
        _launch_record = LaunchRecord(
            task_id=task_id,
            attempt_id=attempt_id,
            agent_name=agent_def.name,
            role=str(agent_def.agent_kind.value or ""),
            prompt_preview=prompt[:500],
        )
        self._publish_mock_record(EventType.MOCK_LAUNCH_RECORDED, _launch_record)
        _prompt_inspection = self._inspect_prompt(
            prompt=prompt,
            agent_def=agent_def,
            metadata=metadata,
        )
        self._publish_mock_record(EventType.MOCK_PROMPT_INSPECTED, _prompt_inspection)
        self._record_initial_messages(
            agent_def=agent_def,
            prompt=prompt,
            metadata=metadata,
        )

        # Publish invocation event.
        if agent_def.name == "entry_executor":
            invocation_type = EventType.ENTRY_EXECUTOR_INVOKED
        elif agent_def.agent_kind.value == "planner":
            invocation_type = EventType.PLANNER_INVOKED
        elif agent_def.agent_kind.value == "executor":
            invocation_type = EventType.EXECUTOR_INVOKED
        elif agent_def.agent_kind.value == "verifier":
            invocation_type = EventType.VERIFIER_INVOKED
        elif agent_def.agent_kind.value == "evaluator":
            invocation_type = EventType.EVALUATOR_INVOKED
        else:
            invocation_type = None

        if invocation_type is not None:
            payload = self._invocation_payload(prompt=prompt, metadata=metadata)
            self._publish(
                invocation_type,
                agent_def=agent_def,
                metadata=metadata,
                payload=payload,
            )

        if agent_def.name == "entry_executor":
            terminal = await self._run_entry_executor(prompt, metadata, emit)
        elif agent_def.agent_kind.value == "planner":
            terminal = await self._run_planner(metadata, emit)
        elif agent_def.agent_kind.value == "executor":
            terminal = await self._run_executor(prompt, metadata, emit)
        elif agent_def.agent_kind.value == "verifier":
            terminal = await self._run_verifier(prompt, metadata, emit)
        elif agent_def.agent_kind.value == "evaluator":
            terminal = await self._run_evaluator(metadata, emit)
        else:
            raise RuntimeError(f"Unsupported mock agent role: {agent_def.agent_kind.value!r}")

        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=terminal,
            agent_name=agent_def.name,
            event_count=1,
        )

    def _metadata_for(
        self,
        *,
        config: Any,
        agent_def: AgentDefinition,
        sandbox_id: str | None,
        extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None,
    ) -> ExecutionMetadata:
        if isinstance(extra_tool_metadata, ExecutionMetadata):
            metadata = extra_tool_metadata.copy()
        else:
            metadata = ExecutionMetadata()
            metadata.update(extra_tool_metadata or {})

        metadata.sandbox_id = str(sandbox_id or metadata.sandbox_id or "")
        metadata.agent_name = agent_def.name
        metadata.repo_root = self._repo_dir
        metadata.cwd = str(getattr(config, "cwd", self._repo_dir) or self._repo_dir)
        metadata.exec_cwd = self._repo_dir
        metadata["role"] = str(agent_def.agent_kind.value or "")
        metadata["agent_type"] = agent_def.agent_type
        metadata["run_id"] = str(metadata.task_center_run_id or "")
        metadata["task_id"] = str(metadata.task_center_task_id or "")
        return metadata

    async def _run_entry_executor(
        self,
        prompt: str,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        goal = self._entry_user_prompt(metadata, fallback=prompt)
        return await self._call_tool(
            submit_execution_handoff,
            {"goal": goal},
            metadata,
            emit,
        )

    async def _run_planner(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        ctx = self._scenario_context(prompt="", metadata=metadata)
        injected = (
            self._mutable_state.consume_next_planner_response()
            if self._mutable_state is not None
            else None
        )
        spec = injected or self._scenario.planner_response(ctx)
        result = await self._call_tool(spec.tool, dict(spec.args), metadata, emit)
        event_type = _PLANNER_EVENT_BY_TOOL.get(spec.tool.name)
        if event_type is not None:
            criteria = list(spec.args.get("evaluation_criteria", ()) or ())
            tasks = list(spec.args.get("tasks", ()) or ())
            self._publish(
                event_type,
                agent_def=None,
                metadata=metadata,
                payload={
                    "task_specification": spec.args.get("task_specification", ""),
                    "evaluation_criteria": criteria,
                    "task_count": len(tasks),
                },
            )
        return result

    async def _run_executor(
        self,
        prompt: str,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        ctx = self._scenario_context(prompt=prompt, metadata=metadata)
        actions = self._scenario.executor_actions(ctx)
        summary = "Workspace preflight completed."
        artifacts: list[str] = []
        for action in actions:
            if isinstance(action, str) and (
                action == "fail" or action.startswith("fail:")
            ):
                reason = (
                    action.split(":", 1)[1]
                    if ":" in action
                    else "Scenario-injected generator failure."
                )
                result = await self._call_tool(
                    submit_execution_failure,
                    {
                        "summary": reason,
                        "reason": reason,
                        "details": [reason],
                    },
                    metadata,
                    emit,
                )
                self._publish(
                    EventType.EXECUTOR_FAILURE,
                    agent_def=None,
                    metadata=metadata,
                    payload={"summary": reason},
                )
                return result
            if isinstance(action, str) and action.startswith(
                "request_recursive_mission:"
            ):
                package_id = action.split(":", 1)[1]
                goal = self._scenario.recursive_mission_goal(ctx) or (
                    f"Resolve recursive package {package_id}."
                )
                result = await self._call_tool(
                    submit_execution_handoff,
                    {"goal": goal},
                    metadata,
                    emit,
                )
                self._publish(
                    EventType.RECURSIVE_MISSION_REQUESTED,
                    metadata=metadata,
                    payload={
                        "package_id": package_id,
                        "goal_id": result.metadata.get("goal_id"),
                    },
                )
                return result
            if isinstance(action, str) and action.startswith(
                "request_recursive_matrix:"
            ):
                package_id = action.split(":", 1)[1]
                goal = self._scenario.recursive_mission_goal(ctx) or (
                    f"Resolve recursive matrix package {package_id}."
                )
                result = await self._call_tool(
                    submit_execution_handoff,
                    {"goal": goal},
                    metadata,
                    emit,
                )
                self._publish(
                    EventType.RECURSIVE_MISSION_REQUESTED,
                    metadata=metadata,
                    payload={
                        "package_id": package_id,
                        "goal_id": result.metadata.get("goal_id"),
                    },
                )
                return result
            if action == "sandbox_integrity":
                await self._run_sandbox_integrity_probe(metadata, emit)
                summary = "Sandbox integrity probe passed."
                artifacts = [self._probe_path()]
            elif action == "final_probe":
                await self._run_final_probe(metadata, emit)
                summary = "Continuation final probe passed."
                artifacts = [self._probe_path()]
            elif action == "preflight":
                await self._run_preflight_probe(metadata, emit)
                summary = "Workspace preflight completed."
                artifacts = []
            elif action == "inspect_user_input":
                script_result = await self._script_engine.run(
                    inspect_user_input_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
            elif isinstance(action, str) and action.startswith("execute_package:"):
                package_id = action.split(":", 1)[1]
                script_result = await self._script_engine.run(
                    execute_package_script(ctx, package_id=package_id),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
            elif action == "final_reconciliation":
                script_result = await self._script_engine.run(
                    final_reconciliation_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
            elif action == "inspect_full_user_input":
                script_result = await self._script_engine.run(
                    inspect_full_user_input_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "occ_conflict_matrix":
                script_result = await self._script_engine.run(
                    occ_conflict_matrix_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "overlay_edge_matrix":
                script_result = await self._script_engine.run(
                    overlay_edge_matrix_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "layerstack_squash_lease":
                script_result = await self._script_engine.run(
                    layerstack_squash_lease_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "lsp_refresh_semantics":
                script_result = await self._script_engine.run(
                    lsp_refresh_semantics_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "recursive_oversized_matrix":
                script_result = await self._script_engine.run(
                    recursive_oversized_matrix_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "full_stack_final_reconciliation":
                script_result = await self._script_engine.run(
                    full_stack_final_reconciliation_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
                self._publish_full_stack_script(script_result.script_name, metadata)
            elif action == "capacity_metrics_full_system":
                script_result = await self._script_engine.run(
                    full_system_capacity_metrics_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
            elif action == "recursive_step":
                script_result = await self._script_engine.run(
                    recursive_step_script(ctx),
                    metadata=metadata,
                    emit=emit,
                )
                summary = script_result.summary
                artifacts = [script_result.artifact]
            elif action == "auto_squash_commit_resume_probe":
                summary_path = await self._run_auto_squash_commit_resume_probe(
                    metadata, emit
                )
                summary = "Auto-squash commit-resume probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build":
                summary_path = await self._run_complex_project_build_probe(
                    metadata, emit, smoke=False
                )
                summary = "Complex project-build probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_smoke":
                summary_path = await self._run_complex_project_build_probe(
                    metadata, emit, smoke=True
                )
                summary = "Complex project-build smoke probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_shell_edit_lsp":
                summary_path = await self._run_complex_project_build_shell_edit_lsp_probe(
                    metadata, emit, smoke=False
                )
                summary = "Complex project-build shell-edit LSP probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_shell_edit_lsp_smoke":
                summary_path = await self._run_complex_project_build_shell_edit_lsp_probe(
                    metadata, emit, smoke=True
                )
                summary = "Complex project-build shell-edit LSP smoke probe passed."
                artifacts = [summary_path]
            else:
                raise RuntimeError(f"Unknown executor action: {action!r}")
        result = await self._call_tool(
            submit_execution_success,
            {"summary": summary, "artifacts": artifacts},
            metadata,
            emit,
        )
        self._publish(
            EventType.EXECUTOR_SUCCESS,
            agent_def=None,
            metadata=metadata,
            payload={"summary": summary},
        )
        return result

    async def _run_verifier(
        self,
        prompt: str,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        ctx = self._scenario_context(prompt=prompt, metadata=metadata)
        rendered_prompt = ctx.rendered_prompt or prompt
        checkpoint = self._spec_field(rendered_prompt, "checkpoint") or "checkpoint"
        if checkpoint == "recursive_return":
            self._publish(
                EventType.RECURSIVE_MISSION_COMPLETED,
                metadata=metadata,
                payload=self._recursive_close_payload(metadata),
            )
        checkpoint_script = (
            full_stack_verifier_checkpoint_script(ctx)
            if self._scenario.name
            in {"full_stack_adversarial", "capacity.full_system_capacity_matrix"}
            else verifier_checkpoint_script(ctx)
        )
        await self._script_engine.run(
            checkpoint_script,
            metadata=metadata,
            emit=emit,
        )
        spec = self._scenario.verifier_response(ctx)
        result = await self._call_tool(spec.tool, dict(spec.args), metadata, emit)
        event_type = _VERIFIER_EVENT_BY_TOOL.get(spec.tool.name)
        if event_type is not None:
            self._publish(
                event_type,
                agent_def=None,
                metadata=metadata,
                payload=self._verifier_payload(rendered_prompt),
            )
        return result

    async def _run_evaluator(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        ctx = self._scenario_context(prompt="", metadata=metadata)
        spec = self._scenario.evaluator_response(ctx)
        result = await self._call_tool(spec.tool, dict(spec.args), metadata, emit)
        event_type = _EVALUATOR_EVENT_BY_TOOL.get(spec.tool.name)
        if event_type is not None:
            self._publish(event_type, agent_def=None, metadata=metadata)
        return result

    def _scenario_context(
        self,
        *,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> ScenarioContext:
        attempt, episode = self._current_attempt_and_episode(metadata)
        runtime = metadata.get("attempt_runtime")
        mission = runtime.mission_store.get(episode.mission_id)
        task_id = str(metadata.get("task_center_task_id") or "")
        task = runtime.task_store.get_task(task_id) if task_id else None
        return ScenarioContext(
            attempt=attempt,
            episode=episode,
            mission=mission,
            prompt=prompt,
            metadata=metadata,
            audit_recorder=self._audit_recorder,
            mutable_state=self._mutable_state,
            task_id=task_id or None,
            agent_name=str(metadata.agent_name or "") or None,
            rendered_prompt=(str(task.get("rendered_prompt") or "") if task else None),
            graph_summary=None,
            requirement_ledger=getattr(self._scenario, "requirement_ledger", None),
            package_plan=getattr(self._scenario, "package_plan", None),
            matrix_plan=getattr(self._scenario, "matrix_plan", None),
        )

    async def _run_preflight_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> None:
        result = await self._call_tool(
            shell_tool,
            {"command": "pwd && git rev-parse --is-inside-work-tree", "timeout": 60},
            metadata,
            emit,
        )
        self._record_tool_check("tool.shell.preflight", result)

    async def _run_sandbox_integrity_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> None:
        probe_dir = ".ephemeralos/sweevo-mock"
        probe_path = self._probe_path()

        mkdir = await self._call_tool(
            shell_tool,
            {
                "command": (
                    f"mkdir -p {probe_dir} && "
                    f"printf 'shell-created\\n' > {probe_dir}/shell.txt"
                ),
                "timeout": 60,
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.shell.gated_merge", mkdir)

        written = await self._call_tool(
            write_file_tool,
            {
                "file_path": probe_path,
                "content": "alpha\nbeta\n",
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.write_file.direct_merge", written)

        first_read = await self._call_tool(
            read_file_tool,
            {"file_path": probe_path, "start_line": 1, "end_line": 20},
            metadata,
            emit,
        )
        self._assert_read_contains(first_read, "alpha", "tool.read_file.after_write")

        edited = await self._call_tool(
            edit_file_tool,
            {
                "file_path": probe_path,
                "old_text": "beta\n",
                "new_text": "beta-edited\n",
                "description": "single edit for mock SWE-EVO probe",
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.edit_file.direct_merge", edited)

        await self._run_batch_edit(metadata, probe_path)
        await self._run_expected_conflict(metadata, probe_path)

        squash = await self._call_tool(
            shell_tool,
            {
                "command": f"printf 'squash-check\\n' >> {probe_path}",
                "timeout": 60,
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.shell.squash_append", squash)

        final_read = await self._call_tool(
            read_file_tool,
            {"file_path": probe_path, "start_line": 1, "end_line": 20},
            metadata,
            emit,
        )
        self._assert_read_contains(final_read, "squash-check", "tool.read_file.after_squash")

    async def _run_batch_edit(
        self,
        metadata: ExecutionMetadata,
        probe_path: str,
    ) -> None:
        sandbox_id = self._require_sandbox_id(metadata)
        result = await sandbox_api.edit_file(
            sandbox_id,
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(old_text="alpha\n", new_text="alpha-batch\n"),
                    SearchReplaceEdit(
                        old_text="beta-edited\n",
                        new_text="beta-batch\n",
                    ),
                ),
                caller=self._caller(metadata),
                description="batch edit for mock SWE-EVO probe",
            ),
            audit_sink=self._sandbox_audit_sink,
        )
        passed = result.success and result.applied_edits == 2
        _sandbox_check = SandboxCheck(
            name="api.edit_file.batch",
            passed=passed,
            detail=f"applied_edits={result.applied_edits} status={result.status}",
            changed_paths=tuple(result.changed_paths),
        )
        self._publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, _sandbox_check)
        if passed:
            self._publish(
                EventType.SANDBOX_BATCH_EDIT_APPLIED,
                metadata=metadata,
                payload={"applied_edits": result.applied_edits},
            )
        if not passed:
            raise RuntimeError("Batch edit did not apply both replacements.")

    async def _run_expected_conflict(
        self,
        metadata: ExecutionMetadata,
        probe_path: str,
    ) -> None:
        sandbox_id = self._require_sandbox_id(metadata)
        result = await sandbox_api.edit_file(
            sandbox_id,
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(
                        old_text="missing-old-text\n",
                        new_text="should-not-apply\n",
                    ),
                ),
                caller=self._caller(metadata),
                description="expected conflict for mock SWE-EVO probe",
            ),
            audit_sink=self._sandbox_audit_sink,
        )
        passed = not result.success
        detail = result.conflict_reason or result.status or "conflict reported"
        _sandbox_check = SandboxCheck(
            name="api.edit_file.conflict_detection",
            passed=passed,
            detail=detail,
            changed_paths=tuple(result.changed_paths),
        )
        self._publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, _sandbox_check)
        if passed:
            self._publish(
                EventType.SANDBOX_CONFLICT_DETECTED,
                metadata=metadata,
                payload={"conflict_reason": detail},
            )
        if not passed:
            raise RuntimeError("Expected conflict edit unexpectedly succeeded.")

    async def _run_auto_squash_commit_resume_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> str:
        # Drives the OCC mutation critical path until layer-stack depth
        # crosses AUTO_SQUASH_MAX_DEPTH (32), then issues edits, reads, a
        # shell readback, and one intentional missing-anchor edit conflict.
        # Captures every tool's timing metadata into a sandbox summary
        # artifact so the paired test can assert on commit_resume_wait_s,
        # auto_squash.total_s, and depth_before > 32.
        probe_dir = ".ephemeralos/sweevo-mock/auto_squash_commit_resume"
        write_count = 36  # AUTO_SQUASH_MAX_DEPTH (32) + 4
        write_paths: list[str] = []
        write_metadata: list[dict[str, Any]] = []
        for index in range(write_count):
            path = f"{probe_dir}/write-{index:02d}.txt"
            content = f"write-{index:02d}\n"
            result = await self._call_tool(
                write_file_tool,
                {"file_path": path, "content": content},
                metadata,
                emit,
            )
            self._record_tool_check(
                f"tool.write_file.depth_seed_{index:02d}", result
            )
            write_paths.append(path)
            write_metadata.append(dict(result.metadata or {}))

        edit_target = f"{probe_dir}/edit-target.txt"
        seed = await self._call_tool(
            write_file_tool,
            {"file_path": edit_target, "content": "alpha=old\nbeta=old\n"},
            metadata,
            emit,
        )
        self._record_tool_check("tool.write_file.edit_seed", seed)
        write_metadata.append(dict(seed.metadata or {}))

        edit_metadata: list[dict[str, Any]] = []
        for index, (old_text, new_text) in enumerate(
            (
                ("alpha=old\n", "alpha=new\n"),
                ("beta=old\n", "beta=new\n"),
            )
        ):
            edit = await self._call_tool(
                edit_file_tool,
                {
                    "file_path": edit_target,
                    "old_text": old_text,
                    "new_text": new_text,
                    "description": (
                        f"auto-squash probe edit {index} after depth threshold"
                    ),
                },
                metadata,
                emit,
            )
            self._record_tool_check(
                f"tool.edit_file.post_threshold_{index}", edit
            )
            edit_metadata.append(dict(edit.metadata or {}))

        first_path = write_paths[0]
        middle_path = write_paths[len(write_paths) // 2]
        last_path = write_paths[-1]
        for label, path in (
            ("first", first_path),
            ("middle", middle_path),
            ("last", last_path),
            ("edited", edit_target),
        ):
            read_result = await self._call_tool(
                read_file_tool,
                {"file_path": path, "start_line": 1, "end_line": 20},
                metadata,
                emit,
            )
            check_name = f"tool.read_file.after_squash_{label}"
            self._record_tool_check(check_name, read_result)

        shell_listing = await self._call_tool(
            shell_tool,
            {
                "command": (
                    f"ls {probe_dir} | sort | head -n 200 && "
                    f"cat {edit_target}"
                ),
                "timeout": 60,
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.shell.readback", shell_listing)

        conflict_result = await self._call_tool(
            edit_file_tool,
            {
                "file_path": edit_target,
                "old_text": "missing-anchor-text\n",
                "new_text": "should-not-apply\n",
                "description": "intentional missing-anchor conflict for auto-squash probe",
            },
            metadata,
            emit,
            allow_error=True,
        )
        conflict_meta = dict(conflict_result.metadata or {})
        conflict_status = str(conflict_meta.get("status") or "")
        conflict_reason = str(conflict_meta.get("conflict_reason") or "")
        conflict_changed_paths = list(conflict_meta.get("changed_paths") or ())
        conflict_passed = bool(conflict_result.is_error and conflict_reason)
        _sandbox_check = SandboxCheck(
            name="tool.edit_file.intentional_conflict",
            passed=conflict_passed,
            detail=(
                f"status={conflict_status} reason={conflict_reason!r} "
                f"is_error={conflict_result.is_error}"
            ),
            changed_paths=tuple(str(p) for p in conflict_changed_paths),
        )
        self._publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, _sandbox_check)
        if not conflict_passed:
            raise RuntimeError(
                "Intentional missing-anchor edit unexpectedly succeeded."
            )
        self._publish(
            EventType.SANDBOX_CONFLICT_DETECTED,
            metadata=metadata,
            payload={"conflict_reason": conflict_reason},
        )

        max_depth_before = 0.0
        max_resume_wait = 0.0
        max_squash_total = 0.0
        for entry in write_metadata + edit_metadata:
            timings = entry.get("timings") or {}
            depth_before = float(timings.get("layer_stack.auto_squash.depth_before", 0.0))
            if depth_before > max_depth_before:
                max_depth_before = depth_before
            resume_wait = float(timings.get("occ.apply.commit_resume_wait_s", 0.0))
            if resume_wait > max_resume_wait:
                max_resume_wait = resume_wait
            squash_total = float(timings.get("layer_stack.auto_squash.total_s", 0.0))
            if squash_total > max_squash_total:
                max_squash_total = squash_total

        summary_path = f"{probe_dir}/summary.json"
        summary_payload = {
            "probe": "auto_squash_commit_resume",
            "write_count": write_count,
            "edit_target": edit_target,
            "edit_paths": write_paths,
            "conflict_status": conflict_status,
            "conflict_reason": conflict_reason,
            "conflict_changed_paths": [str(p) for p in conflict_changed_paths],
            "conflict_is_error": bool(conflict_result.is_error),
            "max_depth_before": max_depth_before,
            "max_commit_resume_wait_s": max_resume_wait,
            "max_auto_squash_total_s": max_squash_total,
        }
        summary_write = await self._call_tool(
            write_file_tool,
            {
                "file_path": summary_path,
                "content": json.dumps(summary_payload, indent=2) + "\n",
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.write_file.summary", summary_write)
        return summary_path

    async def _run_complex_project_build_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        smoke: bool,
    ) -> str:
        from task_center_runner.agent.mock.complex_project_build_probe import (
            run_complex_project_build_probe,
        )

        sandbox_id = self._require_sandbox_id(metadata)
        return await run_complex_project_build_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            publish=self._publish,
            publish_mock_record=self._publish_mock_record,
            record_tool_check=self._record_tool_check,
            caller=self._caller(metadata),
            sandbox_id=sandbox_id,
            smoke=smoke,
        )

    async def _run_complex_project_build_shell_edit_lsp_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        smoke: bool,
    ) -> str:
        from task_center_runner.agent.mock.complex_project_build_shell_edit_lsp_probe import (
            run_complex_project_build_shell_edit_lsp_probe,
        )

        sandbox_id = self._require_sandbox_id(metadata)
        return await run_complex_project_build_shell_edit_lsp_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            publish=self._publish,
            publish_mock_record=self._publish_mock_record,
            record_tool_check=self._record_tool_check,
            caller=self._caller(metadata),
            sandbox_id=sandbox_id,
            smoke=smoke,
        )

    async def _run_final_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> None:
        final_read = await self._call_tool(
            read_file_tool,
            {"file_path": self._probe_path(), "start_line": 1, "end_line": 20},
            metadata,
            emit,
        )
        self._assert_read_contains(final_read, "squash-check", "tool.read_file.final_probe")
        verify = await self._call_tool(
            shell_tool,
            {
                "command": f"grep -q 'squash-check' {self._probe_path()}",
                "timeout": 60,
            },
            metadata,
            emit,
        )
        self._record_tool_check("tool.shell.final_probe", verify)

    async def _call_tool(
        self,
        tool_obj: BaseTool,
        raw_input: dict[str, Any],
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        allow_error: bool = False,
    ) -> ToolResult:
        tool_id = f"toolu_{uuid4().hex}"
        agent_name = str(metadata.agent_name or "")
        run_id = self._stream_run_id(metadata)
        await emit(
            AssistantTextDelta(
                text=f"Calling {tool_obj.name}.\n",
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        await emit(
            AssistantMessageComplete(
                message=ConversationMessage(
                    role="assistant",
                    content=[
                        ToolUseBlock(
                            id=tool_id,
                            name=tool_obj.name,
                            input=dict(raw_input),
                        )
                    ],
                ),
                usage=UsageSnapshot(),
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        await emit(
            ToolExecutionStarted(
                tool_name=tool_obj.name,
                tool_input=dict(raw_input),
                tool_id=tool_id,
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        tool_metadata = metadata.with_overrides(
            tool_id=tool_id,
            sandbox_audit_sink=self._sandbox_audit_sink,
        )
        result = await execute_tool_once(
            tool_obj,
            raw_input,
            ToolExecutionContextService(cwd=Path(self._repo_dir), services=tool_metadata),
            emit=_noop_emit,
            emit_started=False,
        )
        await emit(
            ToolExecutionCompleted(
                tool_name=tool_obj.name,
                output=result.output,
                is_error=result.is_error,
                tool_id=tool_id,
                metadata=dict(result.metadata or {}),
                does_terminate=result.does_terminate,
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        _tool_call_record = ToolCallRecord(
            task_id=str(metadata.get("task_center_task_id") or ""),
            tool_name=tool_obj.name,
            is_error=result.is_error,
            metadata=dict(result.metadata or {}),
        )
        self._publish_mock_record(EventType.MOCK_TOOL_CALL_RECORDED, _tool_call_record)
        if result.is_error and not allow_error:
            raise RuntimeError(f"{tool_obj.name} failed: {result.output}")
        return result

    def _record_tool_check(self, name: str, result: ToolResult) -> None:
        changed_paths = tuple(str(path) for path in result.metadata.get("changed_paths", ()))
        status = str(result.metadata.get("status") or "ok")
        _sandbox_check = SandboxCheck(
            name=name,
            passed=not result.is_error,
            detail=status,
            changed_paths=changed_paths,
        )
        self._publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, _sandbox_check)

    def _assert_read_contains(
        self,
        result: ToolResult,
        needle: str,
        check_name: str,
    ) -> None:
        try:
            payload = json.loads(result.output)
        except json.JSONDecodeError:
            payload = {"content": result.output}
        content = str(payload.get("content") or "")
        passed = needle in content
        _sandbox_check = SandboxCheck(
            name=check_name,
            passed=passed,
            detail=f"needle={needle!r}",
        )
        self._publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, _sandbox_check)
        if not passed:
            raise RuntimeError(f"{check_name} did not find {needle!r}.")

    def _inspect_prompt(
        self,
        *,
        prompt: str,
        agent_def: AgentDefinition,
        metadata: ExecutionMetadata,
    ) -> PromptInspection:
        role = str(agent_def.agent_kind.value or "")
        checks: dict[str, bool]
        reason: str
        if agent_def.name == "entry_executor":
            checks = {
                "entry_request_heading": "# Entry request" in prompt,
                "workspace_root": self._repo_dir in prompt,
                "pr_description": "<pr_description>" in prompt,
            }
            reason = (
                "Entry executor receives the exact SWE-EVO user request as a "
                "required entry_request block before it delegates the mission."
            )
        elif role == "planner":
            attempt, episode = self._current_attempt_and_episode(metadata)
            checks = {
                "mission": "# Mission" in prompt,
                "current_episode": (
                    "# Current Episode" in prompt
                    or "# Mission / Current Episode" in prompt
                ),
            }
            if attempt.attempt_sequence_no > 1:
                checks["failed_attempts"] = (
                    "# Prior Failed Attempts" in prompt
                    or "# Failed Attempts" in prompt
                )
            if episode.sequence_no > 1:
                checks["previous_episode_results"] = "# Previous Episode Results" in prompt
            reason = (
                "Planner context is mission and episode scoped; retry planners "
                "also receive failed-attempt evidence, and continuation planners "
                "receive previous episode results."
            )
        elif role == "executor":
            checks = {
                "attempt_plan": "# Attempt Plan" in prompt,
                "assigned_task": "# Assigned Task" in prompt,
            }
            reason = (
                "Executor context is local to the current planned task with the "
                "attempt contract as framing."
            )
        elif role == "verifier":
            checks = {
                "attempt_plan": "# Attempt Plan" in prompt,
                "assigned_task": "# Assigned Task" in prompt,
            }
            reason = (
                "Verifier context is a generator task profile with the assigned "
                "checkpoint and its dependency evidence."
            )
        elif role == "evaluator":
            checks = {
                "attempt_plan": "# Attempt Plan" in prompt,
                "dependency_results": "# Dependency Results" in prompt,
                "evaluation_criteria": "# Evaluation Criteria" in prompt,
            }
            reason = (
                "Evaluator context is graph-local: attempt contract, completed "
                "generator evidence, and the criteria it must judge."
            )
        else:
            checks = {"known_role": False}
            reason = f"Unknown role {role!r}."

        return PromptInspection(
            task_id=str(metadata.get("task_center_task_id") or ""),
            agent_name=agent_def.name,
            role=role,
            checks=checks,
            justification=reason,
        )

    def _record_initial_messages(
        self,
        *,
        agent_def: AgentDefinition,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> None:
        task_id = str(metadata.get("task_center_task_id") or "")
        if not task_id or self._audit_recorder is None:
            return
        recorder = self._audit_recorder.message_recorder_for_task(task_id)
        if recorder is None:
            return
        recorder.record_initial_messages(
            system_prompt=str(agent_def.system_prompt or ""),
            user_prompt=prompt,
            agent_name=agent_def.name,
            run_id=self._stream_run_id(metadata),
        )

    def _current_attempt_and_episode(
        self,
        metadata: ExecutionMetadata,
    ) -> tuple[Attempt, Episode]:
        runtime = metadata.get("attempt_runtime")
        if runtime is None:
            raise RuntimeError("Missing TrialDeps in mocked agent metadata.")
        attempt_id = str(metadata.get("task_center_attempt_id") or "")
        attempt = runtime.attempt_store.get(attempt_id)
        if attempt is None:
            raise RuntimeError(f"Attempt {attempt_id!r} not found.")
        episode = runtime.episode_store.get(attempt.episode_id)
        if episode is None:
            raise RuntimeError(f"Episode {attempt.episode_id!r} not found.")
        return attempt, episode

    def _probe_path(self) -> str:
        return ".ephemeralos/sweevo-mock/probe.txt"

    def _absolute_probe_path(self, path: str) -> str:
        if path.startswith("/"):
            return path
        return f"{self._repo_dir.rstrip('/')}/{path}"

    @staticmethod
    def _require_sandbox_id(metadata: ExecutionMetadata) -> str:
        sandbox_id = str(metadata.get("sandbox_id") or "").strip()
        if not sandbox_id:
            raise RuntimeError("Sandbox id is required for SWE-EVO sandbox checks.")
        return sandbox_id

    def _caller(self, metadata: ExecutionMetadata) -> SandboxCaller:
        return SandboxCaller(
            agent_id=str(metadata.agent_name or "sweevo-mock"),
            run_id=str(metadata.get("run_id") or ""),
            agent_run_id=str(metadata.agent_run_id or ""),
            task_id=str(metadata.get("task_center_task_id") or ""),
            task_center_run_id=str(metadata.get("task_center_run_id") or ""),
            task_center_task_id=str(metadata.get("task_center_task_id") or ""),
            task_center_attempt_id=str(metadata.get("task_center_attempt_id") or ""),
            task_center_mission_id=str(metadata.get("task_center_mission_id") or ""),
            task_center_request_id=str(metadata.get("task_center_request_id") or ""),
            tool_id=str(metadata.get("tool_id") or ""),
        )

    @staticmethod
    def _stream_run_id(metadata: ExecutionMetadata) -> str:
        return str(
            metadata.get("task_center_task_id")
            or metadata.agent_run_id
            or metadata.get("run_id")
            or ""
        )

    def _entry_user_prompt(
        self,
        metadata: ExecutionMetadata,
        *,
        fallback: str,
    ) -> str:
        runtime = metadata.get("attempt_runtime")
        task_id = str(metadata.get("task_center_task_id") or "")
        if runtime is not None and task_id:
            task = runtime.task_store.get_task(task_id)
            if task is not None:
                rendered_prompt = str(task.get("rendered_prompt") or "")
                if rendered_prompt:
                    return rendered_prompt
        return fallback

    def _invocation_payload(
        self,
        *,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> dict[str, Any]:
        task_id = str(metadata.get("task_center_task_id") or "")
        rendered_prompt = ""
        deps: list[str] = []
        runtime = metadata.get("attempt_runtime")
        if runtime is not None and task_id:
            task = runtime.task_store.get_task(task_id)
            if task is not None:
                rendered_prompt = str(task.get("rendered_prompt") or "")
                deps = [str(item) for item in task.get("needs", [])]
        payload = {
            "task_id": task_id,
            "prompt_preview": prompt[:500],
            "dependency_count": len(deps),
        }
        payload.update(self._verifier_payload(rendered_prompt))
        return payload

    def _verifier_payload(self, rendered_prompt: str) -> dict[str, Any]:
        checkpoint = self._spec_field(rendered_prompt, "checkpoint")
        wave_id = self._spec_field(rendered_prompt, "wave")
        dependency_count = self._spec_field(rendered_prompt, "dependency_count")
        payload: dict[str, Any] = {}
        if checkpoint is not None:
            payload["checkpoint"] = checkpoint
        if wave_id is not None:
            payload["wave_id"] = f"wave_{wave_id}" if wave_id.isdigit() else wave_id
        if dependency_count is not None:
            payload["dependency_count"] = int(dependency_count)
        return payload

    def _recursive_close_payload(self, metadata: ExecutionMetadata) -> dict[str, Any]:
        runtime = metadata.get("attempt_runtime")
        task_id = str(metadata.get("task_center_task_id") or "")
        if runtime is None or not task_id:
            return {}
        verifier_task = runtime.task_store.get_task(task_id)
        if verifier_task is None:
            return {}
        for dep_id in verifier_task.get("needs", []) or []:
            dep_task = runtime.task_store.get_task(str(dep_id))
            if dep_task is None:
                continue
            for summary in dep_task.get("summaries", []) or []:
                payload = summary.get("payload") if isinstance(summary, dict) else None
                if not isinstance(payload, dict):
                    continue
                close_report = payload.get("mission_closure_report")
                if isinstance(close_report, dict):
                    return {
                        "goal_id": close_report.get("goal_id"),
                        "requested_by_task_id": close_report.get(
                            "requested_by_task_id"
                        ),
                        "outcome": close_report.get("outcome"),
                    }
        return {}

    def _publish_full_stack_script(
        self,
        script_name: str,
        metadata: ExecutionMetadata,
    ) -> None:
        self._publish(
            EventType.FULL_STACK_SCRIPT_COMPLETED,
            metadata=metadata,
            payload={"script_name": script_name},
        )

    @staticmethod
    def _spec_field(text: str, name: str) -> str | None:
        prefix = f"{name}="
        for part in text.split():
            if part.startswith(prefix):
                return part[len(prefix) :].strip()
        return None

    def _publish(
        self,
        event_type: EventType,
        *,
        agent_def: AgentDefinition | None = None,
        metadata: ExecutionMetadata | None = None,
        tool_name: str | None = None,
        payload: dict[str, Any] | None = None,
    ) -> None:
        if self._bus is None:
            return
        agent_name: str | None = None
        agent_role: str | None = None
        agent_run_id: str | None = None
        attempt_id: str | None = None
        if agent_def is not None:
            agent_name = agent_def.name or None
            agent_role = str(agent_def.agent_kind.value or "") or None
        if metadata is not None:
            if agent_name is None:
                agent_name = str(metadata.agent_name or "") or None
            agent_run_id = str(metadata.agent_run_id or "") or None
            attempt_id = str(metadata.get("task_center_attempt_id") or "") or None
        node = NodeId(
            task_center_run_id=self._task_center_run_id,
            agent_name=agent_name,
            agent_role=agent_role,  # type: ignore[arg-type]
            agent_run_id=agent_run_id,
            attempt_id=attempt_id,
            tool_name=tool_name,
        )
        self._bus.publish(Event(type=event_type, node=node, payload=payload or {}))

    def _publish_mock_record(
        self, event_type: EventType, record: Any
    ) -> None:
        """Phase 4 — mirror a list-append into the audit bus as a MOCK_* event.

        Dual-write seam: the existing ``self.launches`` / ``self.tool_calls`` /
        ``self.prompt_inspections`` / ``self.sandbox_checks`` lists keep their
        contents (the legacy ``RunReport`` view still reads them), and the same
        record is also emitted as a ``MOCK_*`` ``Event`` so a Phase-4e shim
        subscriber can rebuild those lists from bus events alone — preparing
        for Phase 4g removal of the list attributes.
        """
        if self._bus is None:
            return
        payload = (
            dataclasses.asdict(record)
            if dataclasses.is_dataclass(record) and not isinstance(record, type)
            else dict(record)
        )
        node = NodeId(task_center_run_id=self._task_center_run_id)
        self._bus.publish(Event(type=event_type, node=node, payload=payload))


__all__ = ["MockSquadRunner"]
