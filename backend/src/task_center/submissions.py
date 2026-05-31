"""Validated terminal-outcome submission DTOs (tools ↔ TaskCenter contract).

The ``tools/submission/*`` layer constructs these and hands them to TaskCenter
lifecycle handlers. They are part of the ``task_center`` public facade (see the
package root re-exports), distinct from the internal task vocabulary in
:mod:`task_center._core.task_state`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal


@dataclass(frozen=True, slots=True)
class PlannedGeneratorTask:
    """One normalized generator plan task (role GENERATOR)."""

    local_id: str
    agent_name: str
    needs: tuple[str, ...]
    task_spec: str


@dataclass(frozen=True, slots=True)
class PlannedReducerTask:
    """One normalized reducer plan task (role REDUCER) — the exit gate.

    ``prompt`` is the reducer's contract (required + nonblank); it has no
    ``agent_name`` (reducers always run the ``reducer`` profile).
    """

    local_id: str
    needs: tuple[str, ...]
    prompt: str


@dataclass(frozen=True, slots=True)
class PlannerSubmission:
    """Validated planner submission from a full or partial plan tool."""

    attempt_id: str
    planner_task_id: str
    kind: Literal["completes", "defers"]
    generators: tuple[PlannedGeneratorTask, ...]
    reducers: tuple[PlannedReducerTask, ...]
    deferred_goal_for_next_iteration: str | None


@dataclass(frozen=True, slots=True)
class PlannerFailureSubmission:
    """Runtime-synthesized planner failure."""

    attempt_id: str
    planner_task_id: str
    fail_reason: Literal["run_exhausted"]


@dataclass(frozen=True, slots=True)
class GeneratorSubmission:
    """Validated terminal outcome for one generator task."""

    attempt_id: str
    task_id: str
    status: Literal["success", "failed"]
    outcome: str
    terminal_tool_result: dict[str, Any]


@dataclass(frozen=True, slots=True)
class ReducerSubmission:
    """Validated terminal outcome for one reducer task (binary)."""

    attempt_id: str
    task_id: str
    status: Literal["success", "failed"]
    outcome: str
    terminal_tool_result: dict[str, Any]
