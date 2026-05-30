"""``ScenarioLoopRunner`` — thin AttemptAgentRunner that drives mock agents
through the REAL ``run_ephemeral_agent`` via an injected ``ScenarioEventSource``.

Replaces the imperative ``MockSquadRunner`` engine: instead of hand-executing
tools and hand-emitting lifecycle events, it scripts each agent's turns through
the scenario adapter and lets the real query loop dispatch tools, enforce
terminal-alone, and count budget. It keeps only the harness-observability
responsibilities the report needs:

* ``MOCK_LAUNCH_RECORDED`` (RunReport.launches),
* ``MOCK_TOOL_CALL_RECORDED`` bridged from the loop's
  ``ToolExecutionCompletedEvent``s (RunReport.tool_calls),

and delegates everything else to ``run_ephemeral_agent`` (returning its real
``EphemeralRunResult``). It publishes NO role lifecycle events — workflow shape
is asserted via ``graph_summary``.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any

from message.events import StreamEvent, ToolExecutionCompletedEvent
from tools import ExecutionMetadata

from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.node_id import NodeId
from task_center_runner.agent.mock.event_source import ScenarioEventSource
from task_center_runner.agent.mock.prompt_inspector import (
    LaunchRecord,
    PromptInspection,
    ToolCallRecord,
)
from task_center_runner.agent.mock.scenario_adapter import (
    _attempt_and_iteration,
    scenario_script_for,
)

if TYPE_CHECKING:
    from agents import AgentDefinition
    from runtime.app_factory import RuntimeConfig
    from task_center_runner.audit.bus import AuditEventBus
    from task_center_runner.hooks.registry import MutableMockState
    from task_center_runner.scenarios.base import Scenario


class _UnusedApiClient:
    """``event_source`` short-circuits the loop's provider call, so the api_client
    ``spawn_agent`` builds is never streamed from. A stub keeps spawn cheap and
    avoids requiring live provider credentials."""

    async def aclose(self) -> None:  # pragma: no cover - never used
        return None


def make_mock_runtime_config(repo_dir: str) -> "RuntimeConfig":
    """A ``RuntimeConfig`` for the mock path — carries ``cwd`` + the stub client.

    ``event_source_factory`` is left unset here; ``ScenarioLoopRunner`` sets it
    per run (it needs the scenario/mutable-state it was built with)."""
    from runtime.app_factory import RuntimeConfig

    return RuntimeConfig(cwd=repo_dir, external_api_client=_UnusedApiClient())


class ScenarioLoopRunner:
    """Drop-in ``AttemptAgentRunner`` (same call signature as run_ephemeral_agent)."""

    def __init__(
        self,
        *,
        repo_dir: str,
        bus: "AuditEventBus | None" = None,
        scenario: "Scenario",
        mutable_state: "MutableMockState | None" = None,
        audit_recorder: Any | None = None,
    ) -> None:
        self._repo_dir = repo_dir
        self._bus = bus
        self._scenario = scenario
        self._mutable_state = mutable_state
        self._audit_recorder = audit_recorder

    def bind_audit_recorder(self, audit_recorder: Any | None) -> None:
        self._audit_recorder = audit_recorder

    def _event_source_factory(self, agent_def: "AgentDefinition") -> ScenarioEventSource:
        return ScenarioEventSource(
            script_builder=lambda context: scenario_script_for(
                self._scenario,
                agent_def,
                context,
                mutable_state=self._mutable_state,
                audit_recorder=self._audit_recorder,
                bus=self._bus,
                repo_dir=self._repo_dir,
            ),
            agent_name=agent_def.name,
        )

    async def __call__(
        self,
        config: Any,
        prompt: str,
        *,
        agent_def: "AgentDefinition | None" = None,
        sandbox_id: str | None = None,
        extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
        on_event: Any | None = None,
        initial_messages: Any | None = None,
        task_id: str | None = None,
        persist_agent_run: bool = True,
        **_kwargs: Any,
    ) -> Any:
        from engine.api import run_ephemeral_agent

        if agent_def is None:
            raise RuntimeError("ScenarioLoopRunner requires agent_def.")

        md = extra_tool_metadata or {}
        resolved_task_id = task_id or str(_md_get(md, "task_center_task_id") or "")
        attempt_id = str(_md_get(md, "task_center_attempt_id") or "") or None
        self._publish_launch(agent_def, prompt, resolved_task_id, attempt_id)
        self._publish_prompt_inspection(
            agent_def,
            _combined_user_prompt(prompt, initial_messages),
            md,
        )
        self._record_initial_messages(agent_def, prompt, md, initial_messages)

        config.event_source_factory = self._event_source_factory

        async def bridged_on_event(event: StreamEvent) -> None:
            if isinstance(event, ToolExecutionCompletedEvent):
                self._publish_tool_call(event, resolved_task_id)
            if on_event is not None:
                await on_event(event)

        return await run_ephemeral_agent(
            config,
            prompt,
            agent_def=agent_def,
            sandbox_id=sandbox_id,
            persist_agent_run=persist_agent_run,
            task_id=task_id,
            on_event=bridged_on_event,
            extra_tool_metadata=extra_tool_metadata,
            initial_messages=initial_messages,
        )

    # -- audit-bus records (RunReport observability) ------------------------

    def _publish_launch(
        self,
        agent_def: "AgentDefinition",
        prompt: str,
        task_id: str,
        attempt_id: str | None,
    ) -> None:
        self._publish_record(
            EventType.MOCK_LAUNCH_RECORDED,
            LaunchRecord(
                task_id=task_id,
                attempt_id=attempt_id,
                agent_name=agent_def.name,
                role=str(agent_def.agent_kind.value or ""),
                prompt_preview=prompt[:500],
            ).as_dict(),
        )

    def _publish_tool_call(
        self, event: ToolExecutionCompletedEvent, task_id: str
    ) -> None:
        self._publish_record(
            EventType.MOCK_TOOL_CALL_RECORDED,
            ToolCallRecord(
                task_id=task_id,
                tool_name=event.tool_name,
                is_error=event.is_error,
                metadata=dict(event.metadata or {}),
            ).as_dict(),
        )

    def _publish_record(self, event_type: EventType, payload: dict[str, Any]) -> None:
        if self._bus is None:
            return
        self._bus.publish(
            Event(type=event_type, node=NodeId(task_center_run_id=""), payload=payload)
        )

    # -- prompt inspection + initial-message recording (ported from runner) --

    def _publish_prompt_inspection(
        self, agent_def: "AgentDefinition", prompt: str, metadata: Any
    ) -> None:
        inspection = self._inspect_prompt(
            agent_def=agent_def, prompt=prompt, metadata=metadata
        )
        self._publish_record(
            EventType.MOCK_PROMPT_INSPECTED, dataclasses.asdict(inspection)
        )

    def _inspect_prompt(
        self, *, prompt: str, agent_def: "AgentDefinition", metadata: Any
    ) -> PromptInspection:
        """Verify the launch payload carries the right XML envelopes for the role.

        Only the four squad roles reach ``ScenarioLoopRunner.__call__`` (advisor /
        explorer sub-agents are spawned inside the loop), so those branches cover
        every inspected agent.
        """
        role = str(agent_def.agent_kind.value or "")
        checks: dict[str, bool]
        reason: str
        active_terminals = set(
            _md_get(metadata, "active_terminals") or agent_def.terminals
        )
        if role == "planner" and "submit_plan_defers_goal" not in active_terminals:
            checks = {
                "goal": "<goal>" in prompt,
                "current_iteration": (
                    "<iteration " in prompt and 'position="current"' in prompt
                ),
                "closes_goal_terminal": "submit_plan_closes_goal" in prompt,
                "no_defer_terminal": "submit_plan_defers_goal" not in prompt,
            }
            reason = "Depth-restricted planner exposes only the close-only planner terminal."
        elif role == "planner":
            _attempt, iteration = _attempt_and_iteration(metadata)
            checks = {
                "goal": "<goal>" in prompt,
                "current_iteration": (
                    "<iteration " in prompt and 'position="current"' in prompt
                ),
            }
            # Failed-attempt evidence renders as <attempt attempt_no="k"> when the
            # current iteration has a prior failed attempt; flag it from the prompt
            # (positive-only — a non-retry planner is not penalized) rather than
            # gating on attempt_sequence_no, which the store view may not reflect
            # for the inspected planner.
            if '<attempt attempt_no="' in prompt:
                checks["failed_attempts"] = True
            if iteration.sequence_no > 1:
                checks["previous_iteration_results"] = (
                    'position="prior"' in prompt and "<task " in prompt
                )
            reason = (
                "Planner context is objective and iteration scoped; retry planners also "
                "receive failed-attempt evidence, and continuation planners receive "
                "previous iteration results."
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
                "Evaluator context is graph-local: the active attempt's plan_spec, "
                "per-task outcomes, and the criteria it must judge."
            )
        else:
            checks = {"known_role": False}
            reason = f"Unknown role {role!r}."

        return PromptInspection(
            task_id=str(_md_get(metadata, "task_center_task_id") or ""),
            agent_name=agent_def.name,
            role=role,
            checks=checks,
            justification=reason,
        )

    def _record_initial_messages(
        self,
        agent_def: "AgentDefinition",
        prompt: str,
        metadata: Any,
        seeded_initial_messages: Any,
    ) -> None:
        task_id = str(_md_get(metadata, "task_center_task_id") or "")
        if not task_id or self._audit_recorder is None:
            return
        recorder = self._audit_recorder.message_recorder_for_task(task_id)
        if recorder is None:
            return
        recorder.record_initial_messages(
            system_prompt=str(agent_def.system_prompt or ""),
            user_prompt=prompt,
            agent_name=agent_def.name,
            run_id=_stream_run_id(metadata),
            seeded_initial_messages=list(seeded_initial_messages or []),
            metadata=_initial_message_metadata(metadata),
        )


def _md_get(md: Any, key: str) -> Any:
    getter = getattr(md, "get", None)
    if callable(getter):
        return getter(key)
    return None


def _stream_run_id(md: Any) -> str:
    return str(
        _md_get(md, "task_center_task_id")
        or getattr(md, "agent_run_id", None)
        or _md_get(md, "run_id")
        or ""
    )


def _initial_message_metadata(md: Any) -> dict[str, object]:
    active_terminals = _md_get(md, "active_terminals")
    if not isinstance(active_terminals, (list, tuple, set, frozenset)):
        return {}
    return {"active_terminals": [str(name) for name in active_terminals]}


def _combined_user_prompt(prompt: str, initial_messages: Any | None) -> str:
    parts = [_message_text(message) for message in list(initial_messages or [])]
    parts.append(prompt)
    return "\n\n".join(part for part in parts if part)


def _message_text(message: Any) -> str:
    if isinstance(message, str):
        return message
    content = getattr(message, "content", None)
    if not content:
        return ""
    text_parts: list[str] = []
    for block in content:
        text = getattr(block, "text", None)
        if isinstance(text, str):
            text_parts.append(text)
        elif isinstance(block, dict) and isinstance(block.get("text"), str):
            text_parts.append(block["text"])
    return "".join(text_parts)


__all__ = ["ScenarioLoopRunner", "make_mock_runtime_config"]
