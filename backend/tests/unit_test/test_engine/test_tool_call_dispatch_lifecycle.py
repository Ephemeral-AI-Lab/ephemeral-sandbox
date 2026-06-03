"""Phase 4 §E1/§E2/§AC1–§AC4/§AC6: engine lifecycle batch policy tests."""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import SimpleNamespace

import pytest

from engine.tool_call.dispatch import (
    _dispatch_deferred_tool_calls,
    _record_lifecycle_batch_rejection,
    get_lifecycle_batch_rejection_counters,
    reset_lifecycle_batch_rejection_counters,
)
from message.message import ToolResultBlock, ToolUseBlock
from sandbox._shared.models import Intent
from sandbox.audit import events


def _registry(intent_map: dict[str, Intent]):
    class _Registry:
        def get(self, name):
            intent = intent_map.get(name)
            if intent is None:
                return None
            return SimpleNamespace(intent=intent)

    return _Registry()


def _ctx(
    intent_map: dict[str, Intent],
    *,
    terminal_tools: set[str] | None = None,
) -> SimpleNamespace:
    return SimpleNamespace(
        terminal_tools=terminal_tools or set(),
        tool_metadata=None,
        agent_run_id="run-1",
        tool_registry=_registry(intent_map),
    )


def _tool(name: str, **input_kwargs) -> ToolUseBlock:
    return ToolUseBlock(name=name, input=input_kwargs)


@pytest.fixture(autouse=True)
def _reset_counters_between_tests():
    reset_lifecycle_batch_rejection_counters()
    yield
    reset_lifecycle_batch_rejection_counters()


# ---------------------------------------------------------------------------
# AC1 — single LIFECYCLE + sibling: lifecycle dispatches, sibling rejected.
# ---------------------------------------------------------------------------


def test_tool_call_dispatch_lifecycle_siblings_rejected_lifecycle_executes():
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "write_file": Intent.WRITE_ALLOWED,
    }
    ctx = _ctx(intent_map)
    tool_calls = [_tool("enter_isolated_workspace"), _tool("write_file")]
    tool_results: list[ToolResultBlock] = []
    outcome = _record_lifecycle_batch_rejection(ctx, tool_calls, tool_results)
    assert outcome is not None
    events_emitted, remaining = outcome
    # Lifecycle still dispatchable.
    assert [c.name for c in remaining] == ["enter_isolated_workspace"]
    # Sibling rejected.
    assert len(tool_results) == 1
    rejected_block = tool_results[0]
    assert rejected_block.is_error is True
    assert "enter_isolated_workspace" in rejected_block.content
    assert "write_file" in rejected_block.content
    # One emitted completion event for the rejected sibling.
    assert len(events_emitted) == 1


# ---------------------------------------------------------------------------
# AC2 — multiple LIFECYCLE: all lifecycle calls rejected.
# ---------------------------------------------------------------------------


def test_tool_call_dispatch_multiple_lifecycle_rejected():
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "exit_isolated_workspace": Intent.LIFECYCLE,
        "write_file": Intent.WRITE_ALLOWED,
    }
    ctx = _ctx(intent_map)
    tool_calls = [
        _tool("enter_isolated_workspace"),
        _tool("exit_isolated_workspace"),
        _tool("write_file"),
    ]
    tool_results: list[ToolResultBlock] = []
    outcome = _record_lifecycle_batch_rejection(ctx, tool_calls, tool_results)
    assert outcome is not None
    events_emitted, remaining = outcome
    # Non-lifecycle sibling survives; lifecycle calls rejected.
    assert [c.name for c in remaining] == ["write_file"]
    assert len(tool_results) == 2
    for block in tool_results:
        assert block.is_error is True
        assert "Multiple lifecycle tools" in block.content
    assert len(events_emitted) == 2


# ---------------------------------------------------------------------------
# AC3 — solo lifecycle call passes the gate unchanged.
# ---------------------------------------------------------------------------


def test_tool_call_dispatch_solo_lifecycle_succeeds():
    intent_map = {"enter_isolated_workspace": Intent.LIFECYCLE}
    ctx = _ctx(intent_map)
    tool_calls = [_tool("enter_isolated_workspace")]
    tool_results: list[ToolResultBlock] = []
    outcome = _record_lifecycle_batch_rejection(ctx, tool_calls, tool_results)
    assert outcome is None
    assert tool_results == []


# ---------------------------------------------------------------------------
# AC4 — non-lifecycle batches pass through unchanged.
# ---------------------------------------------------------------------------


def test_tool_call_dispatch_parallel_non_lifecycle_unchanged():
    intent_map = {
        "read_file": Intent.READ_ONLY,
        "grep": Intent.READ_ONLY,
        "write_file": Intent.WRITE_ALLOWED,
    }
    ctx = _ctx(intent_map)
    tool_calls = [_tool("read_file"), _tool("grep"), _tool("write_file")]
    tool_results: list[ToolResultBlock] = []
    outcome = _record_lifecycle_batch_rejection(ctx, tool_calls, tool_results)
    assert outcome is None
    assert tool_results == []


# ---------------------------------------------------------------------------
# AC6 — counter + audit event emitted on rejection.
# ---------------------------------------------------------------------------


def test_lifecycle_batch_rejection_emits_counter_and_audit(tmp_path: Path, monkeypatch):
    audit_path = tmp_path / "lifecycle.jsonl"
    monkeypatch.setenv("EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH", str(audit_path))
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "write_file": Intent.WRITE_ALLOWED,
        "read_file": Intent.READ_ONLY,
    }
    ctx = _ctx(intent_map)
    tool_calls = [
        _tool("enter_isolated_workspace"),
        _tool("write_file"),
        _tool("read_file"),
    ]
    tool_results: list[ToolResultBlock] = []
    outcome = _record_lifecycle_batch_rejection(ctx, tool_calls, tool_results)
    assert outcome is not None

    counters = get_lifecycle_batch_rejection_counters()
    assert counters[("enter_isolated_workspace", "2")] == 1

    assert audit_path.exists()
    contents = audit_path.read_text(encoding="utf-8")
    assert events.WORKSPACE_LIFECYCLE_BATCH_REJECTED in contents
    # Audit payload mentions both rejected siblings.
    assert "write_file" in contents
    assert "read_file" in contents


# ---------------------------------------------------------------------------
# AC1 end-to-end: with a stubbed executor, the lifecycle call actually
# dispatches with ``is_error=False`` and the sibling is rejected.
# ---------------------------------------------------------------------------


def test_tool_call_dispatch_lifecycle_actually_dispatches_when_siblings_rejected(
    monkeypatch,
):
    """End-to-end: lifecycle reaches the dispatcher, sibling does not."""
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "write_file": Intent.WRITE_ALLOWED,
    }
    ctx = _ctx(intent_map)

    dispatched: list[str] = []

    async def _fake_execute(
        context, name, tool_use_id, arguments, *, emit, conversation_messages, consume_budget
    ):
        dispatched.append(name)
        return ToolResultBlock(
            tool_use_id=tool_use_id,
            content="lifecycle ok",
            is_error=False,
        )

    monkeypatch.setattr("engine.tool_call.dispatch.execute_tool_call_streaming", _fake_execute)

    tool_calls = [_tool("enter_isolated_workspace"), _tool("write_file")]
    tool_results: list[ToolResultBlock] = []
    asyncio.run(
        _dispatch_deferred_tool_calls(
            ctx,
            messages=[],
            tool_calls=tool_calls,
            streamed_tool_use_ids=set(),
            background_tasks=None,
            tool_results=tool_results,
        )
    )

    # Sibling rejected, lifecycle dispatched.
    assert dispatched == ["enter_isolated_workspace"]
    by_name = {
        # The rejected sibling produces a ToolResultBlock with the
        # rejection wording.
        "write_file": next(
            block for block in tool_results if "write_file" in str(block.content) and block.is_error
        ),
        # The lifecycle call's dispatch result is appended after rejection.
        "enter_isolated_workspace": next(
            block for block in tool_results if block.content == "lifecycle ok"
        ),
    }
    assert by_name["enter_isolated_workspace"].is_error is False
    assert by_name["write_file"].is_error is True


# ---------------------------------------------------------------------------
# Integration: _dispatch_deferred_tool_calls actually skips the lifecycle
# stage when remaining list is empty (multi-lifecycle without siblings).
# ---------------------------------------------------------------------------


def test_dispatch_deferred_skips_dispatch_when_lifecycle_rejection_drains_batch():
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "exit_isolated_workspace": Intent.LIFECYCLE,
    }
    ctx = _ctx(intent_map)
    tool_calls = [_tool("enter_isolated_workspace"), _tool("exit_isolated_workspace")]
    tool_results: list[ToolResultBlock] = []
    events_out = asyncio.run(
        _dispatch_deferred_tool_calls(
            ctx,
            messages=[],
            tool_calls=tool_calls,
            streamed_tool_use_ids=set(),
            background_tasks=None,
            tool_results=tool_results,
        )
    )
    # Both lifecycle calls rejected; no remaining calls -> no further events.
    assert all(block.is_error for block in tool_results)
    assert len(tool_results) == 2
    # Only completion-from-rejection events are emitted (2).
    assert len(events_out) == 2


# ---------------------------------------------------------------------------
# Sanity: terminal-tool rejection takes precedence (lifecycle stage skipped).
# ---------------------------------------------------------------------------


def test_terminal_rejection_still_runs_before_lifecycle_policy():
    intent_map = {
        "enter_isolated_workspace": Intent.LIFECYCLE,
        "submit_generator_outcome": Intent.WRITE_ALLOWED,
    }
    ctx = _ctx(intent_map, terminal_tools={"submit_generator_outcome"})
    tool_calls = [
        _tool("submit_generator_outcome"),
        _tool("enter_isolated_workspace"),
    ]
    tool_results: list[ToolResultBlock] = []
    events_out = asyncio.run(
        _dispatch_deferred_tool_calls(
            ctx,
            messages=[],
            tool_calls=tool_calls,
            streamed_tool_use_ids=set(),
            background_tasks=None,
            tool_results=tool_results,
        )
    )
    # Terminal batch rejection rejects both calls; lifecycle policy never runs.
    assert len(tool_results) == 2
    assert all("Terminal tool" in block.content for block in tool_results)
    assert len(events_out) == 2


__all__ = ()
