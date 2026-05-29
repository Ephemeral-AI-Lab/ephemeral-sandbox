"""MockSquadRunner — deterministic mock agent execution for live e2e scenarios.

The runner dispatches on ``agent_def.agent_kind`` and calls real submission
tools through ``execute_tool_once``.
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses
import json
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api
from sandbox.shared.clock import monotonic_now
from agents import AgentDefinition
from engine.api import EphemeralRunResult
from message.message import Message, ToolUseBlock
from message.events import (
    AssistantMessageCompleteEvent,
    AssistantTextDeltaEvent,
    StreamEvent,
    ToolExecutionCompletedEvent,
    ToolExecutionStartedEvent,
)
from providers.types import UsageSnapshot
from sandbox.api import (
    EditFileRequest,
    SandboxCaller,
    SearchReplaceEdit,
)
from sandbox.occ.service import AUTO_SQUASH_MAX_DEPTH
from task_center.attempt.state import Attempt
from task_center.iteration.state import Iteration
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
    submit_execution_blocker,
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
    submit_plan_closes_goal,
    submit_plan_defers_goal,
)

from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.legacy import LegacySandboxAuditSink
from task_center_runner.audit.node_id import NodeId
from task_center_runner.scenarios.base import (
    Scenario,
    ScenarioContext,
)
from task_center_runner.scenarios._scenario_helpers import context_message_field
from task_center_runner.hooks.registry import MutableMockState
from task_center_runner.agent.mock._advisor_approval import (
    build_advisor_approval_messages,
)
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
    submit_plan_closes_goal.name: EventType.PLANNER_COMPLETES_GOAL_PLAN,
    submit_plan_defers_goal.name: EventType.PLANNER_DEFERS_GOAL_PLAN,
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


def _initial_message_text(message: Any) -> str:
    """Extract the text payload from a ``Message`` (or dict).

    The mock runner's ``__call__`` accepts ``initial_messages`` as either a
    list of ``Message`` instances (production wiring) or raw
    dicts (some legacy fixtures). Both shapes expose ``content`` as a list
    of blocks with ``type=="text"``.
    """
    content = getattr(message, "content", None)
    if content is None and isinstance(message, dict):
        content = message.get("content")
    if not content:
        return ""
    parts: list[str] = []
    for block in content:
        if hasattr(block, "text"):
            parts.append(str(block.text))
        elif isinstance(block, dict) and block.get("type") == "text":
            parts.append(str(block.get("text", "")))
    return "\n".join(parts)


def _initial_message_metadata(metadata: ExecutionMetadata) -> dict[str, object]:
    active_terminals = metadata.get("active_terminals")
    if not isinstance(active_terminals, (list, tuple, set, frozenset)):
        return {}
    return {"active_terminals": [str(name) for name in active_terminals]}


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
        # Inspect the full wire surface: launch ``prompt`` plus any seeded
        # ``initial_messages``. Post-v3.3 the context envelope lands in
        # ``initial_messages[0]`` and the Task Guidance row lands either in
        # ``prompt`` (3-row launch) or ``initial_messages[1]`` (4-row launch
        # for skill-equipped planners). Checking ``prompt`` alone would miss
        # the ``<context>`` envelope every time.
        seeded_initial = kwargs.get("initial_messages") or []
        wire_payload = "\n".join(
            [prompt] + [_initial_message_text(m) for m in seeded_initial]
        )
        _prompt_inspection = self._inspect_prompt(
            prompt=wire_payload,
            agent_def=agent_def,
            metadata=metadata,
        )
        self._publish_mock_record(EventType.MOCK_PROMPT_INSPECTED, _prompt_inspection)
        self._record_initial_messages(
            agent_def=agent_def,
            prompt=prompt,
            metadata=metadata,
            seeded_initial_messages=seeded_initial,
        )

        # Publish invocation event.
        if agent_def.agent_kind.value == "planner":
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

        if agent_def.agent_kind.value == "planner":
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
        metadata["agent_type"] = agent_def.agent_type.value
        metadata["run_id"] = str(metadata.task_center_run_id or "")
        metadata["task_id"] = str(metadata.task_center_task_id or "")
        return metadata

    def _approve_terminal(
        self,
        metadata: ExecutionMetadata,
        tool: BaseTool,
    ) -> ExecutionMetadata:
        """Return a metadata copy with a synthesized advisor approval prepended.

        The mock squad bypasses the engine loop, so ``conversation_messages``
        arrives empty and every gated terminal would trip
        ``AdvisorApprovalPreHook``. This shim injects the same two-message
        pair the engine would have produced when an agent calls
        ``ask_advisor`` followed by an advisor approval verdict for THIS
        terminal. Negative-path tests can call the helper with a *wrong*
        tool name to confirm the gate still fires when the approval targets
        a different terminal than the one being submitted.
        """
        gated = metadata.copy()
        existing = list(metadata.get("conversation_messages") or [])
        gated["conversation_messages"] = (
            build_advisor_approval_messages(tool_name=tool.name) + existing
        )
        return gated

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
        gated_metadata = self._approve_terminal(metadata, spec.tool)
        result = await self._call_tool(
            spec.tool, dict(spec.args), gated_metadata, emit
        )
        event_type = _PLANNER_EVENT_BY_TOOL.get(spec.tool.name)
        if event_type is not None:
            criteria = list(spec.args.get("evaluation_criteria", ()) or ())
            tasks = list(spec.args.get("tasks", ()) or ())
            self._publish(
                event_type,
                agent_def=None,
                metadata=metadata,
                payload={
                    "plan_spec": spec.args.get("plan_spec", ""),
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
                    submit_execution_blocker,
                    {"summary": reason},
                    self._approve_terminal(metadata, submit_execution_blocker),
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
                "request_recursive_workflow:"
            ):
                package_id = action.split(":", 1)[1]
                goal = self._scenario.recursive_handoff_goal(ctx) or (
                    f"Resolve recursive package {package_id}."
                )
                result = await self._call_tool(
                    submit_execution_handoff,
                    {"goal_handoff": goal},
                    self._approve_terminal(metadata, submit_execution_handoff),
                    emit,
                )
                self._publish(
                    EventType.RECURSIVE_WORKFLOW_REQUESTED,
                    metadata=metadata,
                    payload={
                        "package_id": package_id,
                        "workflow_id": result.metadata.get("workflow_id"),
                    },
                )
                return result
            if isinstance(action, str) and action.startswith(
                "request_recursive_matrix:"
            ):
                package_id = action.split(":", 1)[1]
                goal = self._scenario.recursive_handoff_goal(ctx) or (
                    f"Resolve recursive matrix package {package_id}."
                )
                result = await self._call_tool(
                    submit_execution_handoff,
                    {"goal_handoff": goal},
                    self._approve_terminal(metadata, submit_execution_handoff),
                    emit,
                )
                self._publish(
                    EventType.RECURSIVE_WORKFLOW_REQUESTED,
                    metadata=metadata,
                    payload={
                        "package_id": package_id,
                        "workflow_id": result.metadata.get("workflow_id"),
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
            elif action == "high_concurrency_seed":
                summary_path = await self._run_high_concurrency_seed_probe(
                    metadata, emit
                )
                summary = "High-concurrency sandbox seed passed."
                artifacts = [summary_path]
            elif isinstance(action, str) and action.startswith(
                "high_concurrency_worker:"
            ):
                worker_index = int(action.split(":", 1)[1])
                summary_path = await self._run_high_concurrency_worker_probe(
                    metadata,
                    emit,
                    index=worker_index,
                )
                summary = f"High-concurrency worker {worker_index:02d} passed."
                artifacts = [summary_path]
            elif action == "high_concurrency_reconcile":
                summary_path = await self._run_high_concurrency_reconcile_probe(
                    metadata, emit
                )
                summary = "High-concurrency sandbox reconciliation passed."
                artifacts = [summary_path]
            elif action == "heavy_io_zoned_seed":
                summary_path = await self._run_heavy_io_zoned_seed_probe(
                    metadata, emit
                )
                summary = "Heavy-IO zoned seed passed."
                artifacts = [summary_path]
            elif isinstance(action, str) and action.startswith(
                "heavy_io_zoned_worker:"
            ):
                worker_index = int(action.split(":", 1)[1])
                summary_path = await self._run_heavy_io_zoned_worker_probe(
                    metadata,
                    emit,
                    index=worker_index,
                )
                summary = f"Heavy-IO zoned worker {worker_index:02d} passed."
                artifacts = [summary_path]
            elif action == "heavy_io_zoned_reconcile":
                summary_path = await self._run_heavy_io_zoned_reconcile_probe(
                    metadata, emit
                )
                summary = "Heavy-IO zoned reconciliation passed."
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
            elif action == "complex_project_build_shell_edit_lsp_shared_bootstrap":
                summary_path = await self._run_complex_project_build_shell_edit_lsp_probe(
                    metadata,
                    emit,
                    smoke=False,
                    shared_attempt_bootstrap=True,
                )
                summary = "Complex project-build shell-edit LSP probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_shell_edit_lsp_smoke":
                summary_path = await self._run_complex_project_build_shell_edit_lsp_probe(
                    metadata, emit, smoke=True
                )
                summary = "Complex project-build shell-edit LSP smoke probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_grep_glob":
                summary_path = await self._run_complex_project_build_grep_glob_probe(
                    metadata, emit, smoke=False
                )
                summary = "Complex project-build grep/glob probe passed."
                artifacts = [summary_path]
            elif action == "complex_project_build_grep_glob_smoke":
                summary_path = await self._run_complex_project_build_grep_glob_probe(
                    metadata, emit, smoke=True
                )
                summary = "Complex project-build grep/glob smoke probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_all_verbs":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="all_verbs"
                )
                summary = "Ephemeral-workspace all-verbs probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_concurrent_writes":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="concurrent_writes"
                )
                summary = "Ephemeral-workspace concurrent-writes probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_same_path_conflict":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="same_path_conflict"
                )
                summary = "Ephemeral-workspace same-path conflict probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_policy":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="policy"
                )
                summary = "Ephemeral-workspace policy probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_cancellation":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="cancellation"
                )
                summary = "Ephemeral-workspace cancellation probe passed."
                artifacts = [summary_path]
            elif action == "ephemeral_workspace_o1_disk":
                summary_path = await self._run_ephemeral_workspace_probe(
                    metadata, emit, mode="o1_disk"
                )
                summary = "Ephemeral-workspace O(1) disk probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_golden":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="golden"
                )
                summary = "Background-shell golden probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_stop":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="cancel"
                )
                summary = "Background-shell cancel probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_interleave":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="interleave"
                )
                summary = "Background-shell interleave probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_exhaustion":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="exhaustion"
                )
                summary = "Background-shell exhaustion probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_partial_write_cancel":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="partial_write_cancel"
                )
                summary = "Background-shell partial-write-cancel probe passed."
                artifacts = [summary_path]
            elif action == "background_shell_stop_during_maintenance":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="cancel_during_maintenance"
                )
                summary = (
                    "Background-shell cancel-during-maintenance probe passed."
                )
                artifacts = [summary_path]
            elif action == "background_shell_late_cancel_race":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="late_cancel_race"
                )
                summary = "Background-shell late-cancel-race probe passed."
                artifacts = [summary_path]
            elif action == "background_mixed_fg_bg_same_path_conflict":
                summary_path = await self._run_background_shell_probe(
                    metadata,
                    emit,
                    mode="mixed_fg_bg_same_path_conflict",
                )
                summary = "Background-shell mixed foreground/background conflict probe passed."
                artifacts = [summary_path]
            elif action == "background_heartbeat_loss_reaps_only_stale_bg":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="heartbeat_loss"
                )
                summary = "Background-shell heartbeat-loss probe passed."
                artifacts = [summary_path]
            elif action == "background_exit_iws_drains_agent_tasks":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="exit_iws_drain"
                )
                summary = "Background-shell isolated-workspace drain probe passed."
                artifacts = [summary_path]
            elif action == "background_engine_restart_no_lease_leak":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="engine_restart_no_lease_leak"
                )
                summary = "Background-shell engine-restart cleanup probe passed."
                artifacts = [summary_path]
            elif action == "background_many_small_writes_do_not_starve_dispatcher":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="many_small_writes"
                )
                summary = "Background-shell many-small-writes probe passed."
                artifacts = [summary_path]
            elif action == "background_mixed_op_concurrent":
                summary_path = await self._run_background_shell_probe(
                    metadata, emit, mode="mixed_op_concurrent"
                )
                summary = "Background-shell mixed-op concurrent probe passed."
                artifacts = [summary_path]
            elif action == "plugin_read_only_lsp_refresh":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="read_only_lsp_refresh"
                )
                summary = "Plugin READ_ONLY LSP refresh probe passed."
                artifacts = [summary_path]
            elif action == "plugin_write_allowed_publish":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="write_allowed_publish"
                )
                summary = "Plugin WRITE_ALLOWED publish probe passed."
                artifacts = [summary_path]
            elif action == "plugin_intent_contract":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="intent_contract"
                )
                summary = "Plugin intent contract probe passed."
                artifacts = [summary_path]
            elif action == "plugin_iws_policy":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="iws_policy"
                )
                summary = "Plugin isolated-workspace policy probe passed."
                artifacts = [summary_path]
            elif action == "plugin_setup_failure":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="setup_failure"
                )
                summary = "Plugin setup failure probe passed."
                artifacts = [summary_path]
            elif action == "plugin_service_evict":
                summary_path = await self._run_plugin_workspace_probe(
                    metadata, emit, mode="service_evict"
                )
                summary = "Plugin service eviction probe passed."
                artifacts = [summary_path]
            else:
                raise RuntimeError(f"Unknown executor action: {action!r}")
        result = await self._call_tool(
            submit_execution_success,
            {"summary": summary, "artifacts": artifacts},
            self._approve_terminal(metadata, submit_execution_success),
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
        context_message = ctx.context_message or prompt
        checkpoint = context_message_field(context_message, "checkpoint") or "checkpoint"
        if checkpoint == "recursive_return":
            self._publish(
                EventType.RECURSIVE_WORKFLOW_COMPLETED,
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
        gated_metadata = self._approve_terminal(metadata, spec.tool)
        result = await self._call_tool(
            spec.tool, dict(spec.args), gated_metadata, emit
        )
        event_type = _VERIFIER_EVENT_BY_TOOL.get(spec.tool.name)
        if event_type is not None:
            self._publish(
                event_type,
                agent_def=None,
                metadata=metadata,
                payload=self._verifier_payload(context_message),
            )
        return result

    async def _run_evaluator(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolResult:
        ctx = self._scenario_context(prompt="", metadata=metadata)
        spec = self._scenario.evaluator_response(ctx)
        gated_metadata = self._approve_terminal(metadata, spec.tool)
        result = await self._call_tool(
            spec.tool, dict(spec.args), gated_metadata, emit
        )
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
        attempt, iteration = self._current_attempt_and_iteration(metadata)
        runtime = metadata.get("attempt_runtime")
        workflow = runtime.workflow_store.get(iteration.workflow_id)
        task_id = str(metadata.get("task_center_task_id") or "")
        task = runtime.task_store.get_task(task_id) if task_id else None
        return ScenarioContext(
            attempt=attempt,
            iteration=iteration,
            workflow=workflow,
            prompt=prompt,
            metadata=metadata,
            audit_recorder=self._audit_recorder,
            mutable_state=self._mutable_state,
            task_id=task_id or None,
            agent_name=str(metadata.agent_name or "") or None,
            context_message=(str(task.get("context_message") or "") if task else None),
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
        # crosses AUTO_SQUASH_MAX_DEPTH, then issues edits, reads, a
        # shell readback, and one intentional missing-anchor edit conflict.
        # Captures every tool's timing metadata into a sandbox summary
        # artifact so the paired test can assert on commit_resume_wait_s,
        # auto_squash.total_s, and depth_before > AUTO_SQUASH_MAX_DEPTH.
        probe_dir = ".ephemeralos/sweevo-mock/auto_squash_commit_resume"
        write_count = AUTO_SQUASH_MAX_DEPTH + 4
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

    async def _run_high_concurrency_seed_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> str:
        from task_center_runner.agent.mock.high_concurrency_probe import (
            run_high_concurrency_seed_probe,
        )

        return await run_high_concurrency_seed_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            record_tool_check=self._record_tool_check,
        )

    async def _run_high_concurrency_worker_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        index: int,
    ) -> str:
        from task_center_runner.agent.mock.high_concurrency_probe import (
            run_high_concurrency_worker_probe,
        )

        return await run_high_concurrency_worker_probe(
            index=index,
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            publish=self._publish,
            publish_mock_record=self._publish_mock_record,
            record_tool_check=self._record_tool_check,
        )

    async def _run_high_concurrency_reconcile_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> str:
        from task_center_runner.agent.mock.high_concurrency_probe import (
            run_high_concurrency_reconcile_probe,
        )

        return await run_high_concurrency_reconcile_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            record_tool_check=self._record_tool_check,
        )

    async def _run_heavy_io_zoned_seed_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> str:
        from task_center_runner.agent.mock.heavy_io_zoned_probe import (
            run_heavy_io_zoned_seed_probe,
        )

        return await run_heavy_io_zoned_seed_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            record_tool_check=self._record_tool_check,
        )

    async def _run_heavy_io_zoned_worker_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        index: int,
    ) -> str:
        from task_center_runner.agent.mock.heavy_io_zoned_probe import (
            run_heavy_io_zoned_worker_probe,
        )

        return await run_heavy_io_zoned_worker_probe(
            index=index,
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            publish=self._publish,
            publish_mock_record=self._publish_mock_record,
            record_tool_check=self._record_tool_check,
        )

    async def _run_heavy_io_zoned_reconcile_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> str:
        from task_center_runner.agent.mock.heavy_io_zoned_probe import (
            run_heavy_io_zoned_reconcile_probe,
        )

        return await run_heavy_io_zoned_reconcile_probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            record_tool_check=self._record_tool_check,
        )

    async def _run_background_shell_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        mode: str,
    ) -> str:
        from task_center_runner.agent.mock import background_shell_probe

        dispatch = {
            "golden": background_shell_probe.run_background_shell_golden_probe,
            "cancel": background_shell_probe.run_background_shell_stop_probe,
            "interleave": background_shell_probe.run_background_shell_interleave_probe,
            "exhaustion": background_shell_probe.run_background_shell_exhaustion_probe,
            "partial_write_cancel": (
                background_shell_probe.run_background_shell_partial_write_cancel_probe
            ),
            "cancel_during_maintenance": (
                background_shell_probe.run_background_shell_maintenance_probe
            ),
            "late_cancel_race": (
                background_shell_probe.run_background_shell_late_cancel_probe
            ),
            "mixed_fg_bg_same_path_conflict": (
                background_shell_probe.run_background_mixed_fg_bg_same_path_conflict_probe
            ),
            "heartbeat_loss": (
                background_shell_probe.run_background_heartbeat_loss_probe
            ),
            "exit_iws_drain": (
                background_shell_probe.run_background_exit_iws_drains_agent_tasks_probe
            ),
            "engine_restart_no_lease_leak": (
                background_shell_probe.run_background_engine_restart_no_lease_leak_probe
            ),
            "many_small_writes": (
                background_shell_probe.run_background_many_small_writes_probe
            ),
            "mixed_op_concurrent": (
                background_shell_probe.run_background_mixed_op_concurrent_probe
            ),
        }
        probe = dispatch.get(mode)
        if probe is None:
            raise RuntimeError(f"unknown background_shell probe mode: {mode!r}")
        return await probe(
            metadata=metadata,
            emit=emit,
            call_tool=self._call_tool,
            record_tool_check=self._record_tool_check,
        )

    async def _run_ephemeral_workspace_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        mode: str,
    ) -> str:
        from task_center_runner.agent.mock import ephemeral_workspace_probe

        sandbox_id = self._require_sandbox_id(metadata)
        dispatch = {
            "all_verbs": ephemeral_workspace_probe.run_ephemeral_all_verbs_probe,
            "concurrent_writes": (
                ephemeral_workspace_probe.run_ephemeral_concurrent_writes_probe
            ),
            "same_path_conflict": (
                ephemeral_workspace_probe.run_ephemeral_same_path_conflict_probe
            ),
            "policy": ephemeral_workspace_probe.run_ephemeral_policy_probe,
            "cancellation": ephemeral_workspace_probe.run_ephemeral_cancellation_probe,
            "o1_disk": ephemeral_workspace_probe.run_ephemeral_o1_disk_probe,
        }
        probe = dispatch.get(mode)
        if probe is None:
            raise RuntimeError(f"unknown ephemeral_workspace probe mode: {mode!r}")
        kwargs: dict[str, Any] = {
            "metadata": metadata,
            "emit": emit,
            "call_tool": self._call_tool,
            "record_tool_check": self._record_tool_check,
        }
        if mode != "same_path_conflict":
            kwargs["sandbox_id"] = sandbox_id
        return await probe(**kwargs)

    async def _run_plugin_workspace_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        mode: str,
    ) -> str:
        from task_center_runner.agent.mock import plugin_workspace_probe

        sandbox_id = self._require_sandbox_id(metadata)
        dispatch = {
            "read_only_lsp_refresh": (
                plugin_workspace_probe.run_plugin_read_only_lsp_refresh_probe
            ),
            "write_allowed_publish": (
                plugin_workspace_probe.run_plugin_write_allowed_publish_probe
            ),
            "intent_contract": plugin_workspace_probe.run_plugin_intent_contract_probe,
            "iws_policy": plugin_workspace_probe.run_plugin_iws_policy_probe,
            "setup_failure": plugin_workspace_probe.run_plugin_setup_failure_probe,
            "service_evict": plugin_workspace_probe.run_plugin_service_evict_probe,
        }
        probe = dispatch.get(mode)
        if probe is None:
            raise RuntimeError(f"unknown plugin workspace probe mode: {mode!r}")
        kwargs: dict[str, Any] = {
            "metadata": metadata,
            "emit": emit,
            "call_tool": self._call_tool,
            "record_tool_check": self._record_tool_check,
        }
        if mode != "intent_contract":
            kwargs["sandbox_id"] = sandbox_id
        return await probe(**kwargs)

    async def _run_complex_project_build_shell_edit_lsp_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        smoke: bool,
        shared_attempt_bootstrap: bool = False,
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
            shared_attempt_bootstrap=shared_attempt_bootstrap,
        )

    async def _run_complex_project_build_grep_glob_probe(
        self,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        smoke: bool,
    ) -> str:
        from task_center_runner.agent.mock.complex_project_build_grep_glob_probe import (
            run_complex_project_build_grep_glob_probe,
        )

        sandbox_id = self._require_sandbox_id(metadata)
        return await run_complex_project_build_grep_glob_probe(
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
        background_task_id: str | None = None,
        sandbox_invocation_id: str | None = None,
    ) -> ToolResult:
        tool_use_id = f"toolu_{uuid4().hex}"
        agent_name = str(metadata.agent_name or "")
        run_id = self._stream_run_id(metadata)
        await emit(
            AssistantTextDeltaEvent(
                text=f"Calling {tool_obj.name}.\n",
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        await emit(
            AssistantMessageCompleteEvent(
                message=Message(
                    role="assistant",
                    content=[
                        ToolUseBlock(
                            id=tool_use_id,
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
        # Capture client-side wallclock around the tool dispatch boundary so a
        # stall that does not show up in the sandbox-side ``api.*.total_s``
        # timings can be localized. ``emit_started_usec`` covers the audit emit
        # path (bus + recorder file IO); ``execute_tool_once_usec`` covers the
        # whole tool body including pre-hooks, parse, and the sandbox RPC.
        # Keys end in ``_usec`` not ``_s`` so they do not get picked up by the
        # ``_looks_like_duration`` heuristic that already over-counts nested
        # timings in the sandbox-event duration aggregate.
        client_t0 = monotonic_now()
        await emit(
            ToolExecutionStartedEvent(
                tool_name=tool_obj.name,
                tool_input=dict(raw_input),
                tool_use_id=tool_use_id,
                agent_name=agent_name,
                run_id=run_id,
            )
        )
        client_t1 = monotonic_now()
        override_kwargs: dict[str, Any] = {
            "tool_use_id": tool_use_id,
            "sandbox_audit_sink": self._sandbox_audit_sink,
        }
        resolved_sandbox_invocation_id = ""
        if background_task_id is not None:
            resolved_sandbox_invocation_id = sandbox_invocation_id or uuid4().hex
        if background_task_id is not None:
            override_kwargs["background_task_id"] = background_task_id
            override_kwargs["sandbox_invocation_id"] = resolved_sandbox_invocation_id
        tool_metadata = metadata.with_overrides(**override_kwargs)
        try:
            result = await execute_tool_once(
                tool_obj,
                raw_input,
                ToolExecutionContextService(cwd=Path(self._repo_dir), services=tool_metadata),
                emit=_noop_emit,
                emit_started=False,
            )
        except asyncio.CancelledError:
            if resolved_sandbox_invocation_id and metadata.sandbox_id:
                current_task = asyncio.current_task()
                if current_task is not None:
                    current_task.uncancel()
                with contextlib.suppress(Exception):
                    await sandbox_api.cancel(
                        metadata.sandbox_id,
                        resolved_sandbox_invocation_id,
                    )
                self._publish(
                    EventType.SANDBOX_TOOL_CANCELLED,
                    metadata=metadata,
                    tool_name=tool_obj.name,
                    payload={
                        "tool_name": tool_obj.name,
                        "tool_use_id": tool_use_id,
                        "invocation_id": resolved_sandbox_invocation_id,
                        "background_task_id": background_task_id,
                    },
                )
            raise
        client_t2 = monotonic_now()
        result_metadata = dict(result.metadata or {})
        client_timings = dict(result_metadata.get("timings") or {})
        client_timings["mock.client.emit_started_usec"] = (client_t1 - client_t0) * 1_000_000
        client_timings["mock.client.execute_tool_once_usec"] = (
            client_t2 - client_t1
        ) * 1_000_000
        client_timings["mock.client.dispatch_wallclock_usec"] = (
            client_t2 - client_t0
        ) * 1_000_000
        result_metadata["timings"] = client_timings
        result = dataclasses.replace(result, metadata=result_metadata)
        await emit(
            ToolExecutionCompletedEvent(
                tool_name=tool_obj.name,
                output=result.output,
                is_error=result.is_error,
                tool_use_id=tool_use_id,
                metadata=dict(result.metadata or {}),
                is_terminal=result.is_terminal,
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
        """Verify the spawn's launch payload carries the right XML envelopes.

        Post v3.3 the wire shape is XML-tagged blocks wrapped in a
        ``<context>...</context>`` envelope (row 2) plus a
        ``<Task Guidance>...</Task Guidance>`` envelope (row 3). The
        inspector checks for the tag opens — not the old markdown headings.
        """
        role = str(agent_def.agent_kind.value or "")
        checks: dict[str, bool]
        reason: str
        active_terminals = set(metadata.get("active_terminals") or agent_def.terminals)
        if role == "planner" and "submit_plan_defers_goal" not in active_terminals:
            checks = {
                "goal": "<goal>" in prompt,
                "current_iteration": (
                    "<iteration " in prompt and 'position="current"' in prompt
                ),
                "closes_goal_terminal": "submit_plan_closes_goal" in prompt,
                "no_defer_terminal": "submit_plan_defers_goal" not in prompt,
            }
            reason = (
                "Depth-restricted planner exposes only the closes-goal terminal."
            )
        elif role == "planner":
            attempt, iteration = self._current_attempt_and_iteration(metadata)
            checks = {
                "goal": "<goal>" in prompt,
                "current_iteration": (
                    "<iteration " in prompt and 'position="current"' in prompt
                ),
            }
            if attempt.attempt_sequence_no > 1:
                checks["failed_attempts"] = '<attempt attempt_no="' in prompt
            if iteration.sequence_no > 1:
                checks["previous_iteration_results"] = (
                    'position="prior"' in prompt and "<task " in prompt
                )
            reason = (
                "Planner context is goal and iteration scoped; retry planners "
                "also receive failed-attempt evidence, and continuation planners "
                "receive previous iteration results."
            )
        elif role == "executor":
            checks = {
                "plan_spec": "<plan_spec>" in prompt,
                "assigned_task": "<assigned_task" in prompt,
            }
            reason = (
                "Executor context is local to the current planned task with the "
                "attempt contract as framing."
            )
        elif role == "verifier":
            checks = {
                "plan_spec": "<plan_spec>" in prompt,
                "assigned_task": "<assigned_task" in prompt,
            }
            reason = (
                "Verifier context is a generator task profile with the assigned "
                "checkpoint and its dependency evidence."
            )
        elif role == "evaluator":
            checks = {
                "plan_spec": "<plan_spec>" in prompt,
                "task_outcomes": "<task " in prompt,
                "evaluation_criteria": "<evaluation_criteria>" in prompt,
            }
            reason = (
                "Evaluator context is graph-local: the active attempt's "
                "plan_spec, per-task outcomes, and the criteria it must judge."
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
        seeded_initial_messages: list[Message] | None = None,
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
            seeded_initial_messages=list(seeded_initial_messages or []),
            metadata=_initial_message_metadata(metadata),
        )

    def _current_attempt_and_iteration(
        self,
        metadata: ExecutionMetadata,
    ) -> tuple[Attempt, Iteration]:
        runtime = metadata.get("attempt_runtime")
        if runtime is None:
            raise RuntimeError("Missing AttemptDeps in mocked agent metadata.")
        attempt_id = str(metadata.get("task_center_attempt_id") or "")
        attempt = runtime.attempt_store.get(attempt_id)
        if attempt is None:
            raise RuntimeError(f"Attempt {attempt_id!r} not found.")
        iteration = runtime.iteration_store.get(attempt.iteration_id)
        if iteration is None:
            raise RuntimeError(f"Iteration {attempt.iteration_id!r} not found.")
        return attempt, iteration

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
            task_center_workflow_id=str(metadata.get("task_center_workflow_id") or ""),
            task_center_request_id=str(metadata.get("task_center_request_id") or ""),
            tool_id=str(metadata.get("tool_use_id") or ""),
        )

    @staticmethod
    def _stream_run_id(metadata: ExecutionMetadata) -> str:
        return str(
            metadata.get("task_center_task_id")
            or metadata.agent_run_id
            or metadata.get("run_id")
            or ""
        )

    def _invocation_payload(
        self,
        *,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> dict[str, Any]:
        task_id = str(metadata.get("task_center_task_id") or "")
        context_message = ""
        deps: list[str] = []
        runtime = metadata.get("attempt_runtime")
        if runtime is not None and task_id:
            task = runtime.task_store.get_task(task_id)
            if task is not None:
                context_message = str(task.get("context_message") or "")
                deps = [str(item) for item in task.get("needs", [])]
        payload = {
            "task_id": task_id,
            "prompt_preview": prompt[:500],
            "dependency_count": len(deps),
        }
        payload.update(self._verifier_payload(context_message))
        return payload

    def _verifier_payload(self, context_message: str) -> dict[str, Any]:
        checkpoint = context_message_field(context_message, "checkpoint")
        wave_id = context_message_field(context_message, "wave")
        dependency_count = context_message_field(
            context_message, "dependency_count"
        )
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
                close_report = payload.get("workflow_closure_report")
                if isinstance(close_report, dict):
                    return {
                        "workflow_id": close_report.get("workflow_id"),
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
        """Publish one mock-runner record to the audit bus."""
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
