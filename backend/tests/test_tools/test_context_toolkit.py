"""Tests for tools.context.toolkit and freshness helpers."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from message.messages import SystemReminderBlock
from team.runtime.scope_change_buffer import (
    SCOPE_CHANGE_CATEGORY,
    SCOPE_CHANGE_SUPERSEDED,
    ScopeChangeBuffer,
)
from tools.context.toolkit import ContextChangedSinceTool
from tools.core.base import ToolExecutionContext


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


@pytest.mark.asyncio
async def test_context_changed_since_marks_checked_and_excludes_own_run_changes():
    own_change = SimpleNamespace(
        file_path="src/auth/local.py",
        agent_id="developer",
        agent_run_id="run-1",
    )
    other_change = SimpleNamespace(
        file_path="src/auth/session.py",
        agent_id="peer",
        agent_run_id="run-2",
    )
    ctx = _ctx(
        {
            "work_item_started_at": 1.0,
            "agent_run_id": "run-1",
            "write_scope": ["src/auth/"],
            "file_change_store": SimpleNamespace(
                initialized=True,
                changes_since=lambda _since: [own_change, other_change],
            ),
        }
    )

    result = await ContextChangedSinceTool().execute(
        ContextChangedSinceTool.input_model(),
        ctx,
    )

    payload = json.loads(result.output)
    assert payload["scope_changes_by_others"] == 1
    assert payload["stale"] is True
    assert ctx.metadata["checked_context_freshness"] is True


def test_scope_change_buffer_returns_text_and_supersedes_old_message():
    buf = ScopeChangeBuffer(min_turns_between=1)
    display_messages = []

    buf.buffer({"file_path": "src/auth/a.py", "edit_type": "edit", "agent_id": "peer"})
    first_text = buf.flush_into(display_messages)

    assert first_text is not None
    assert len(display_messages) == 1
    first_block = display_messages[0].content[0]
    assert isinstance(first_block, SystemReminderBlock)
    assert first_block.category == SCOPE_CHANGE_CATEGORY

    buf.buffer({"file_path": "src/auth/b.py", "edit_type": "write", "agent_id": "peer-2"})
    second_text = buf.flush_into(display_messages)

    assert second_text is not None
    assert len(display_messages) == 2
    assert display_messages[0].content[0].category == SCOPE_CHANGE_SUPERSEDED
    assert display_messages[1].content[0].category == SCOPE_CHANGE_CATEGORY


@pytest.mark.asyncio
async def test_context_changed_since_ignores_unrelated_sibling_completion():
    class _Dispatcher:
        async def done_sibling_ids(self, **_kwargs):
            return ["sib-1"]

        async def get_task_by_id(self, _task_id):
            return SimpleNamespace(scope_paths=["src/payments/"])

    ctx = _ctx(
        {
            "work_item_started_at": 1.0,
            "work_item_id": "task-1",
            "task_parent_id": "parent-1",
            "write_scope": ["src/auth/"],
            "dispatcher": _Dispatcher(),
        }
    )

    result = await ContextChangedSinceTool().execute(
        ContextChangedSinceTool.input_model(),
        ctx,
    )

    payload = json.loads(result.output)
    assert payload["stale"] is False
    assert payload["new_sibling_completions"] == 0


@pytest.mark.asyncio
async def test_context_changed_since_counts_overlapping_sibling_completion():
    class _Dispatcher:
        async def done_sibling_ids(self, **_kwargs):
            return ["sib-1"]

        async def get_task_by_id(self, _task_id):
            return SimpleNamespace(scope_paths=["src/auth/session.py"])

    ctx = _ctx(
        {
            "work_item_started_at": 1.0,
            "work_item_id": "task-1",
            "task_parent_id": "parent-1",
            "write_scope": ["src/auth/"],
            "dispatcher": _Dispatcher(),
        }
    )

    result = await ContextChangedSinceTool().execute(
        ContextChangedSinceTool.input_model(),
        ctx,
    )

    payload = json.loads(result.output)
    assert payload["stale"] is True
    assert payload["new_sibling_completions"] == 1
