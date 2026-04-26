"""Tests for the mode-entry tools (US-005).

Covers:
- Happy path: enter_plan_for_handoff / enter_prepare_continue_to_work mutate
  ``Task.mode`` and return a briefing with ``mode_transition`` set.
- Subagent rejection.
- Role mismatch rejection.
- Idempotent re-entry when already in the target mode.
- Cross-secondary rejection (already in another secondary mode).
- Batch exclusivity via ``validate_tool_batch``.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from engine.core.tool_batch import validate_tool_batch
from message.messages import ToolUseBlock
from task_center.task import Status, Task
from tools.core.base import ToolExecutionContextService, ToolRegistry
from tools.core.runtime import ExecutionMetadata
from tools.mode_tool.enter_plan_for_handoff import enter_plan_for_handoff
from tools.mode_tool.enter_prepare_continue_to_work import (
    enter_prepare_continue_to_work,
)


# --------------------------------------------------------------------------- #
# Fakes                                                                       #
# --------------------------------------------------------------------------- #


@dataclass
class _FakeGraph:
    task: Task

    def get(self, _id: str) -> Task:
        return self.task


@dataclass
class _FakeTC:
    graph: _FakeGraph


def _make_task(role: str = "executor", mode: str = "direct") -> Task:
    t = Task(
        id="t1",
        role=role,  # type: ignore[arg-type]
        title="title",
        spec="spec",
        status=Status.RUNNING,
    )
    t.mode = mode
    return t


def _ctx(
    *,
    role: str = "executor",
    agent_type: str = "agent",
    task: Task | None = None,
) -> ToolExecutionContextService:
    meta = ExecutionMetadata()
    meta["role"] = role
    meta["agent_type"] = agent_type
    if task is not None:
        meta["task_center"] = _FakeTC(graph=_FakeGraph(task=task))
        meta["task_id"] = "t1"
    return ToolExecutionContextService(cwd=Path("/tmp"), services=meta)


# --------------------------------------------------------------------------- #
# enter_plan_for_handoff                                                      #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_plan_entry_happy_path_mutates_mode() -> None:
    task = _make_task(role="executor", mode="direct")
    res = await enter_plan_for_handoff._entrypoint(context=_ctx(task=task))
    assert not res.is_error
    assert res.mode_transition == "plan_for_handoff"
    assert "You have entered plan_for_handoff mode" in res.output
    assert "submit_plan_handoff with a well-formed plan" in res.output
    assert task.mode == "plan_for_handoff"


@pytest.mark.asyncio
async def test_plan_entry_rejects_subagent_context() -> None:
    task = _make_task(role="executor", mode="direct")
    res = await enter_plan_for_handoff._entrypoint(
        context=_ctx(task=task, agent_type="subagent"),
    )
    assert res.is_error
    assert "subagent" in res.output
    assert task.mode == "direct"  # unchanged


@pytest.mark.asyncio
async def test_plan_entry_rejects_evaluator_role() -> None:
    task = _make_task(role="executor", mode="direct")
    res = await enter_plan_for_handoff._entrypoint(
        context=_ctx(task=task, role="evaluator"),
    )
    assert res.is_error
    assert "executor-only" in res.output
    assert task.mode == "direct"


@pytest.mark.asyncio
async def test_plan_entry_idempotent_when_already_in_mode() -> None:
    task = _make_task(role="executor", mode="plan_for_handoff")
    res = await enter_plan_for_handoff._entrypoint(context=_ctx(task=task))
    assert not res.is_error
    assert res.mode_transition == "plan_for_handoff"
    assert task.mode == "plan_for_handoff"  # still in the same mode


@pytest.mark.asyncio
async def test_plan_entry_rejects_cross_secondary_transition() -> None:
    task = _make_task(role="executor", mode="prepare_continue_to_work")
    res = await enter_plan_for_handoff._entrypoint(context=_ctx(task=task))
    assert res.is_error
    assert "cross-secondary" in res.output
    assert task.mode == "prepare_continue_to_work"  # unchanged


@pytest.mark.asyncio
async def test_plan_entry_missing_task_center_metadata() -> None:
    """Without task_center / task_id, the tool reports the metadata gap."""
    res = await enter_plan_for_handoff._entrypoint(context=_ctx(role="executor"))
    assert res.is_error
    assert "missing" in res.output


# --------------------------------------------------------------------------- #
# enter_prepare_continue_to_work                                              #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_prepare_entry_happy_path_mutates_mode() -> None:
    task = _make_task(role="evaluator", mode="direct")
    res = await enter_prepare_continue_to_work._entrypoint(
        context=_ctx(task=task, role="evaluator"),
    )
    assert not res.is_error
    assert res.mode_transition == "prepare_continue_to_work"
    assert "You have entered prepare_continue_to_work mode" in res.output
    assert "submit_continue_work_handoff with continuation input" in res.output
    assert task.mode == "prepare_continue_to_work"


@pytest.mark.asyncio
async def test_prepare_entry_rejects_executor_role() -> None:
    task = _make_task(role="evaluator", mode="direct")
    res = await enter_prepare_continue_to_work._entrypoint(
        context=_ctx(task=task, role="executor"),
    )
    assert res.is_error
    assert "evaluator-only" in res.output
    assert task.mode == "direct"


@pytest.mark.asyncio
async def test_prepare_entry_idempotent() -> None:
    task = _make_task(role="evaluator", mode="prepare_continue_to_work")
    res = await enter_prepare_continue_to_work._entrypoint(
        context=_ctx(task=task, role="evaluator"),
    )
    assert not res.is_error
    assert res.mode_transition == "prepare_continue_to_work"


@pytest.mark.asyncio
async def test_prepare_entry_rejects_cross_secondary_transition() -> None:
    """Already in plan_for_handoff (e.g. via misconfigured agent), can't enter."""
    task = _make_task(role="evaluator", mode="plan_for_handoff")
    res = await enter_prepare_continue_to_work._entrypoint(
        context=_ctx(task=task, role="evaluator"),
    )
    assert res.is_error
    assert "cross-secondary" in res.output


# --------------------------------------------------------------------------- #
# Batch exclusivity                                                           #
# --------------------------------------------------------------------------- #


@dataclass
class _BatchCtx:
    tool_registry: ToolRegistry
    terminal_tools: set


def test_entry_tool_rejects_when_batched_with_sibling() -> None:
    registry = ToolRegistry()
    registry.register(enter_plan_for_handoff)
    ctx = _BatchCtx(tool_registry=registry, terminal_tools=set())

    calls: list[Any] = [
        ToolUseBlock(id="a", name="enter_plan_for_handoff", input={}),
        ToolUseBlock(id="b", name="some_other_tool", input={}),
    ]
    res = validate_tool_batch(ctx, calls)  # type: ignore[arg-type]
    assert res is not None and len(res) == 2
    assert all(r.is_error for r in res)
    assert "Mode-entry tool" in res[0].content


def test_entry_tool_alone_in_batch_is_ok() -> None:
    registry = ToolRegistry()
    registry.register(enter_plan_for_handoff)
    ctx = _BatchCtx(tool_registry=registry, terminal_tools=set())

    res = validate_tool_batch(
        ctx,  # type: ignore[arg-type]
        [ToolUseBlock(id="a", name="enter_plan_for_handoff", input={})],
    )
    assert res is None


def test_terminal_and_entry_in_same_batch_both_flagged() -> None:
    registry = ToolRegistry()
    registry.register(enter_plan_for_handoff)
    ctx = _BatchCtx(
        tool_registry=registry,
        terminal_tools={"submit_task_completion"},
    )

    res = validate_tool_batch(
        ctx,  # type: ignore[arg-type]
        [
            ToolUseBlock(id="a", name="enter_plan_for_handoff", input={}),
            ToolUseBlock(id="b", name="submit_task_completion", input={}),
        ],
    )
    assert res is not None and len(res) == 2
    assert "Terminal/mode-entry" in res[0].content


# --------------------------------------------------------------------------- #
# Cross-secondary deny payload includes current mode's terminals (spec §FM)   #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_cross_secondary_deny_lists_current_mode_terminals() -> None:
    """Spec §Failure Modes: 'Tool returns is_error=true *listing the existing
    mode's allowed terminals*.' The cross-secondary deny payload must name
    the terminals of the mode the task is currently stuck in."""
    from agents.types import AgentDefinition, ModeDefinition

    # Build a synthetic agent with two secondary modes so a cross-secondary
    # transition is structurally possible.
    synthetic = AgentDefinition(
        name="dual_secondary",
        description="d",
        role="evaluator",
        modes=[
            ModeDefinition(
                name="direct",
                is_default=True,
                terminals=["submit_task_completion"],
            ),
            ModeDefinition(
                name="plan_for_handoff",
                allowed_tools=[],
                terminals=["submit_plan_handoff"],
                entry_tool="enter_plan_for_handoff",
                briefing="b1",
            ),
            ModeDefinition(
                name="prepare_continue_to_work",
                allowed_tools=[],
                terminals=["submit_continue_work_handoff"],
                entry_tool="enter_prepare_continue_to_work",
                briefing="b2",
            ),
        ],
    )

    task = _make_task(role="evaluator", mode="plan_for_handoff")
    meta = ExecutionMetadata()
    meta["role"] = "evaluator"
    meta["agent_type"] = "agent"
    meta["task_center"] = _FakeTC(graph=_FakeGraph(task=task))
    meta["task_id"] = "t1"
    meta["agent_def"] = synthetic
    ctx = ToolExecutionContextService(cwd=Path("/tmp"), services=meta)

    res = await enter_prepare_continue_to_work._entrypoint(context=ctx)
    assert res.is_error
    assert "cross-secondary" in res.output
    assert "submit_plan_handoff" in res.output  # the current mode's terminal
    assert "plan_for_handoff" in res.output
    assert task.mode == "plan_for_handoff"  # unchanged


@pytest.mark.asyncio
async def test_cross_secondary_deny_falls_back_when_agent_def_absent() -> None:
    """Without ``agent_def`` in metadata the helper can't enumerate terminals
    — it should still produce a structured deny instead of crashing."""
    task = _make_task(role="evaluator", mode="plan_for_handoff")
    res = await enter_prepare_continue_to_work._entrypoint(
        context=_ctx(task=task, role="evaluator"),
    )
    assert res.is_error
    assert "cross-secondary" in res.output
    assert "(none registered)" in res.output
