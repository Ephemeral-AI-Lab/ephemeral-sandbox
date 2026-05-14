"""Scenario protocol + ScenarioContext + ToolCallSpec + CompositeScenario.

Per plan §10. Scenarios are pure descriptions; the squad runner translates
them into actual tool calls. The protocol is intentionally narrow — the four
decision methods plus a ``hooks()`` declaration.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Protocol, runtime_checkable

from live_e2e.audit.events import EventType
from live_e2e.hooks.registry import Hook


@dataclass(frozen=True, slots=True)
class ToolCallSpec:
    """Description of an agent submission tool call."""

    tool: Any  # BaseTool
    args: dict[str, Any]


@dataclass(slots=True)
class ScenarioContext:
    """Live state visible to a scenario at a decision point."""

    attempt: Any  # Attempt | None
    episode: Any  # Episode | None
    mission: Any  # Mission | None
    prompt: str
    metadata: Any  # ExecutionMetadata
    audit_recorder: Any  # AuditRecorder | None
    mutable_state: Any  # MutableMockState | None
    task_id: str | None = None
    agent_name: str | None = None
    rendered_prompt: str | None = None
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

    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None: ...

    def hooks(self) -> Sequence[Hook]: ...


@dataclass(frozen=True, slots=True)
class CompositeScenario:
    """Trivial composition placeholder — phase-1 ships only one scenario.

    The fields and ``compose`` classmethod are exposed so next-phase code can
    bolt actual composition logic onto this surface without breaking imports.
    """

    parts: tuple[Scenario, ...]


class ScenarioBase:
    """Default implementation of the Scenario protocol.

    Subclasses override the four decision methods. ``hooks()`` defaults to no
    hooks. ``compose`` returns a :class:`CompositeScenario` carrying the parts.
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

    def recursive_mission_goal(self, ctx: ScenarioContext) -> str | None:  # noqa: ARG002
        return None

    def hooks(self) -> Sequence[Hook]:
        return ()

    @classmethod
    def compose(cls, *parts: Scenario) -> CompositeScenario:
        return CompositeScenario(parts=tuple(parts))


__all__ = [
    "CompositeScenario",
    "Scenario",
    "ScenarioBase",
    "ScenarioContext",
    "ToolCallSpec",
]
