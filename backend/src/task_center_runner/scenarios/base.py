"""Scenario protocol + ScenarioContext + ToolCallSpec.

Scenarios are pure descriptions; the mock runner translates them into actual
tool calls. The protocol is intentionally narrow: planner/executor/verifier/
evaluator decisions, optional recursive-handoff goal text, and hooks.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Protocol, runtime_checkable

from task_center_runner.audit.events import EventType
from task_center_runner.hooks.registry import Hook


@dataclass(frozen=True, slots=True)
class ToolCallSpec:
    """Description of an agent submission tool call."""

    tool: Any  # BaseTool
    args: dict[str, Any]


@dataclass(slots=True)
class ScenarioContext:
    """Live state visible to a scenario at a decision point."""

    attempt: Any  # Attempt | None
    iteration: Any  # Iteration | None
    workflow: Any  # Workflow | None
    prompt: str
    metadata: Any  # ExecutionMetadata
    audit_recorder: Any  # AuditRecorder | None
    mutable_state: Any  # MutableMockState | None
    task_id: str | None = None
    agent_name: str | None = None
    context_message: str | None = None
    graph_summary: dict[str, Any] | None = None
    requirement_ledger: Any = None
    package_plan: Any = None
    matrix_plan: Any = None


@runtime_checkable
class Scenario(Protocol):
    """A scenario that drives one mock-agent run end-to-end."""

    name: str
    expected_event_sequence: tuple[EventType, ...]

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[Any]: ...

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec: ...

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None: ...

    def hooks(self) -> Sequence[Hook]: ...


class ScenarioBase:
    """Default implementation of the Scenario protocol.

    Subclasses override the decision methods they need. ``hooks()`` defaults to
    no hooks.
    """

    name: str = ""
    expected_event_sequence: tuple[EventType, ...] = ()

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        raise NotImplementedError

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[Any]:  # noqa: ARG002
        return ()

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        raise NotImplementedError

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        raise NotImplementedError

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return None

    def hooks(self) -> Sequence[Hook]:
        return ()


__all__ = [
    "Scenario",
    "ScenarioBase",
    "ScenarioContext",
    "ToolCallSpec",
]
