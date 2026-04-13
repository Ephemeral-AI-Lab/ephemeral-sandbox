"""Unit tests for team.task_center.TaskCenter."""

from __future__ import annotations

import asyncio
import time
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.models import BudgetConfig, BudgetState, Note, Task, TaskStatus
from team.task_center import TaskCenter


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


class _FakeSessionFactory:
    """No-op session factory for tests that only exercise in-memory notes."""
    def __call__(self):
        class _Ctx:
            async def __aenter__(self_inner):
                return None
            async def __aexit__(self_inner, *a):
                return False
        return _Ctx()


class _RecordingStore:
    """Collect TeamRun events appended by TaskCenter."""

    def __init__(self) -> None:
        self.events = []

    def append(self, event) -> None:
        self.events.append(event)

    def load_run(self, team_run_id: str):
        return list(self.events)

    def list_runs(self):
        return ["run-1"] if self.events else []


def _tc(**kwargs) -> TaskCenter:
    """Create a TaskCenter with test defaults for required params."""
    defaults = dict(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    defaults.update(kwargs)
    return TaskCenter(**defaults)


def _note(
    id_: str,
    task_id: str,
    content: str = "some content",
    *,
    agent_name: str = "developer",
    timestamp: float | None = None,
    scope_paths: list[str] | None = None,
    parent_note_id: str | None = None,
) -> Note:
    return Note(
        id=id_,
        task_id=task_id,
        agent_name=agent_name,
        content=content,
        timestamp=timestamp if timestamp is not None else time.time(),
        scope_paths=scope_paths or [],
        parent_note_id=parent_note_id,
    )


def _task(
    id_: str,
    task: str = "do work",
    deps: list[str] | None = None,
    scope_paths: list[str] | None = None,
    parent_id: str | None = None,
) -> Task:
    return Task(
        id=id_,
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.PENDING,
        task=task,
        deps=deps or [],
        scope_paths=scope_paths or [],
        parent_id=parent_id,
    )


def _run(awaitable):
    return asyncio.run(awaitable)


# ---------------------------------------------------------------------------
# Basic post / read
# ---------------------------------------------------------------------------


def test_empty_task_center_returns_empty_reads():
    tc = _tc()
    assert _run(tc.read()) == []


def test_post_appends_notes():
    tc = _tc()
    n1 = _note("n1", "task-1", "hello")
    n2 = _note("n2", "task-2", "world")
    _run(tc.post(n1))
    _run(tc.post(n2))
    notes = _run(tc.read())
    assert len(notes) == 2
    assert notes[0].id == "n1"
    assert notes[1].id == "n2"


def test_post_emits_note_posted_event():
    store = _RecordingStore()
    tc = _tc(event_store=store)

    _run(tc.post(_note(
        "n1",
        "task-1",
        "first line\nsecond line",
        agent_name="developer (auto)",
        scope_paths=["src/auth"],
    )))

    assert len(store.events) == 1
    event = store.events[0]
    assert event.kind == "note_posted"
    assert event.data["task_id"] == "task-1"
    assert event.data["agent_name"] == "developer (auto)"
    assert event.data["auto"] is True
    assert event.data["scope_paths"] == ["src/auth"]
    assert event.data["content_preview"] == "first line second line"


def test_post_logs_auto_note(caplog):
    tc = _tc()

    with caplog.at_level("INFO", logger="team.task_center"):
        _run(tc.post(_note("n1", "task-1", "checkpoint summary", agent_name="developer (auto)")))

    assert "[task_center] auto-note task=task-1" in caplog.text


# ---------------------------------------------------------------------------
# Filtering: authors
# ---------------------------------------------------------------------------


def test_read_filters_by_task_id():
    tc = _tc()
    _run(tc.post(_note("n1", "task-A")))
    _run(tc.post(_note("n2", "task-B")))
    _run(tc.post(_note("n3", "task-A")))

    results = _run(tc.read(authors=["task-A"]))
    assert len(results) == 2
    assert all(n.task_id == "task-A" for n in results)


def test_read_authors_multiple():
    tc = _tc()
    _run(tc.post(_note("n1", "task-A")))
    _run(tc.post(_note("n2", "task-B")))
    _run(tc.post(_note("n3", "task-C")))

    results = _run(tc.read(authors=["task-A", "task-C"]))
    assert {n.task_id for n in results} == {"task-A", "task-C"}


def test_read_authors_no_match_returns_empty():
    tc = _tc()
    _run(tc.post(_note("n1", "task-A")))
    assert _run(tc.read(authors=["task-Z"])) == []


# ---------------------------------------------------------------------------
# Filtering: scope_paths (prefix matching)
# ---------------------------------------------------------------------------


def test_read_scope_paths_prefix_match():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1", scope_paths=["src/auth/session.py"])))
    _run(tc.post(_note("n2", "task-2", scope_paths=["src/billing/invoice.py"])))

    results = _run(tc.read(scope_paths=["src/auth"]))
    assert len(results) == 1
    assert results[0].id == "n1"


def test_read_scope_paths_exact_match():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1", scope_paths=["src/auth"])))
    results = _run(tc.read(scope_paths=["src/auth"]))
    assert len(results) == 1


def test_read_scope_paths_no_scope_on_note_includes_note():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1")))
    results = _run(tc.read(scope_paths=["src/auth"]))
    assert [note.id for note in results] == ["n1"]


def test_read_scope_paths_trailing_slash_stripped():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1", scope_paths=["src/auth/session.py"])))
    results = _run(tc.read(scope_paths=["src/auth/"]))
    assert len(results) == 1


def test_read_scope_paths_matches_broader_note_scope_from_narrow_query():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1", scope_paths=["src/auth"])))
    results = _run(tc.read(scope_paths=["src/auth/session.py"]))
    assert len(results) == 1
    assert results[0].id == "n1"


def test_read_scope_paths_respects_component_boundaries():
    tc = _tc()
    _run(tc.post(_note("n1", "task-1", scope_paths=["src/authz.py"])))
    assert _run(tc.read(scope_paths=["src/auth"])) == []


# ---------------------------------------------------------------------------
# Filtering: since
# ---------------------------------------------------------------------------


def test_read_since_filters_by_timestamp():
    tc = _tc()
    _run(tc.post(_note("n1", "t1", timestamp=100.0)))
    _run(tc.post(_note("n2", "t2", timestamp=200.0)))
    _run(tc.post(_note("n3", "t3", timestamp=300.0)))

    results = _run(tc.read(since=200.0))
    assert len(results) == 2
    assert {n.id for n in results} == {"n2", "n3"}


def test_read_since_none_returns_all():
    tc = _tc()
    _run(tc.post(_note("n1", "t1", timestamp=100.0)))
    _run(tc.post(_note("n2", "t2", timestamp=200.0)))
    assert len(_run(tc.read(since=None))) == 2


# ---------------------------------------------------------------------------
# Filtering: limit
# ---------------------------------------------------------------------------


def test_read_limit_returns_last_n():
    tc = _tc()
    for i in range(5):
        _run(tc.post(_note(f"n{i}", f"t{i}")))

    results = _run(tc.read(limit=3))
    assert len(results) == 3
    assert results[0].id == "n2"
    assert results[-1].id == "n4"


def test_read_limit_larger_than_total_returns_all():
    tc = _tc()
    _run(tc.post(_note("n1", "t1")))
    results = _run(tc.read(limit=100))
    assert len(results) == 1


# ---------------------------------------------------------------------------
# Combined filters
# ---------------------------------------------------------------------------


def test_read_combined_authors_and_since():
    tc = _tc()
    _run(tc.post(_note("n1", "task-A", timestamp=100.0)))
    _run(tc.post(_note("n2", "task-A", timestamp=300.0)))
    _run(tc.post(_note("n3", "task-B", timestamp=300.0)))

    results = _run(tc.read(authors=["task-A"], since=200.0))
    assert len(results) == 1
    assert results[0].id == "n2"


def test_read_combined_scope_and_limit():
    tc = _tc()
    _run(tc.post(_note("n1", "t1", scope_paths=["src/auth/a.py"])))
    _run(tc.post(_note("n2", "t2", scope_paths=["src/auth/b.py"])))
    _run(tc.post(_note("n3", "t3", scope_paths=["src/auth/c.py"])))

    results = _run(tc.read(scope_paths=["src/auth"], limit=2))
    assert len(results) == 2
    assert results[0].id == "n2"
    assert results[1].id == "n3"


# ---------------------------------------------------------------------------
# context_for
# ---------------------------------------------------------------------------


def test_context_for_always_includes_task_section():
    tc = _tc()
    task = _task("work-1", task="implement login flow")
    ctx = _run(tc.context_for(task))
    assert "## Your task" in ctx
    assert "implement login flow" in ctx


def test_context_for_includes_scope_paths_when_present():
    tc = _tc()
    task = _task("work-1", task="do auth", scope_paths=["src/auth/"])
    ctx = _run(tc.context_for(task))
    assert "Scope:" in ctx
    assert "src/auth/" in ctx


def test_context_for_no_scope_paths_omits_scope_line():
    tc = _tc()
    task = _task("work-1", task="general work")
    ctx = _run(tc.context_for(task))
    assert "Scope:" not in ctx


def test_context_for_includes_dep_notes_when_deps_exist():
    tc = _tc()
    _run(tc.post(_note("n1", "dep-task", "dependency output", agent_name="developer")))
    task = _task("work-1", task="build on dep", deps=["dep-task"])
    ctx = _run(tc.context_for(task))
    assert "Context from dependencies" in ctx
    assert "dependency output" in ctx


def test_context_for_dep_notes_absent_when_no_deps():
    tc = _tc()
    _run(tc.post(_note("n1", "unrelated", "some output")))
    task = _task("work-1", task="standalone work")
    ctx = _run(tc.context_for(task))
    assert "Context from dependencies" not in ctx


def test_context_for_includes_parent_notes_when_parent_id_matches():
    tc = _tc()
    _run(tc.post(_note("n1", "parent-task", "parent reasoning", agent_name="team_planner")))
    task = _task("work-1", task="child task", parent_id="parent-task")

    # Mock get_task so _parent_chain_ids doesn't hit DB
    parent = _task("parent-task", task="parent")
    async def _mock_get_task(task_id):
        return parent if task_id == "parent-task" else None
    tc.get_task = _mock_get_task

    ctx = _run(tc.context_for(task))
    assert "Parent context" in ctx
    assert "parent reasoning" in ctx


def test_context_for_walks_parent_chain_via_internal_get_task():
    tc = _tc()
    _run(tc.post(_note("n1", "root-task", "root rationale", agent_name="team_planner")))
    _run(tc.post(_note("n2", "parent-task", "parent reasoning", agent_name="team_planner")))
    task = _task("work-1", task="child task", parent_id="parent-task")

    # Mock get_task to simulate parent chain without DB
    parent = _task("parent-task", task="parent", parent_id="root-task")
    root = _task("root-task", task="root")
    async def _mock_get_task(task_id):
        if task_id == "parent-task":
            return parent
        if task_id == "root-task":
            return root
        return None
    tc.get_task = _mock_get_task

    ctx = _run(tc.context_for(task))
    assert "root rationale" in ctx
    assert "parent reasoning" in ctx


def test_context_for_no_parent_notes_when_parent_id_is_none():
    tc = _tc()
    _run(tc.post(_note("n1", "some-task", "context")))
    task = _task("work-1", task="root level task")
    ctx = _run(tc.context_for(task))
    assert "Parent context" not in ctx


def test_context_for_respects_max_context_bytes():
    tc = _tc()
    big_content = "x" * 100_000
    _run(tc.post(_note("n1", "dep-task", big_content, agent_name="developer")))
    task = _task("work-1", task="build on dep", deps=["dep-task"])

    ctx = _run(tc.context_for(task, max_context_bytes=500))
    assert "## Your task" in ctx
    assert len(ctx.encode()) < 100_000


def test_context_for_task_section_never_trimmed():
    tc = _tc()
    big_content = "z" * 200_000
    _run(tc.post(_note("n1", "dep-task", big_content)))
    task = _task("work-1", task="important task description", deps=["dep-task"])
    ctx = _run(tc.context_for(task, max_context_bytes=100))
    assert "important task description" in ctx


def test_context_for_includes_recent_scope_changes_from_file_change_store():
    file_change_store = SimpleNamespace(
        initialized=True,
        changes_since=lambda since: [
            SimpleNamespace(
                file_path="src/auth/session.py",
                edit_type="edit",
                agent_id="reviewer",
                created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
            ),
            SimpleNamespace(
                file_path="src/billing/invoice.py",
                edit_type="edit",
                agent_id="reviewer",
                created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
            ),
        ],
    )
    tc = _tc(file_change_store=file_change_store)
    task = _task("work-1", task="do auth", scope_paths=["src/auth/"])
    task.created_at = datetime(2026, 4, 12, 12, 0, tzinfo=timezone.utc)

    ctx = _run(tc.context_for(task))

    assert "## Recent changes in your scope" in ctx
    assert "src/auth/session.py" in ctx
    assert "src/billing/invoice.py" not in ctx


# ---------------------------------------------------------------------------
# snapshot / restore
# ---------------------------------------------------------------------------


def test_snapshot_returns_copy_of_notes():
    tc = _tc()
    _run(tc.post(_note("n1", "t1")))
    _run(tc.post(_note("n2", "t2")))

    snap = tc.snapshot()
    assert len(snap) == 2
    assert snap is not tc._notes


def test_snapshot_copy_is_independent():
    tc = _tc()
    _run(tc.post(_note("n1", "t1")))
    snap = tc.snapshot()
    _run(tc.post(_note("n2", "t2")))
    assert len(snap) == 1
    assert len(_run(tc.read())) == 2


def test_restore_replaces_notes():
    tc = _tc()
    _run(tc.post(_note("n1", "t1")))
    _run(tc.post(_note("n2", "t2")))

    backup = tc.snapshot()
    _run(tc.post(_note("n3", "t3")))
    assert len(_run(tc.read())) == 3

    tc.restore(backup)
    assert len(_run(tc.read())) == 2
    assert _run(tc.read())[0].id == "n1"
    assert _run(tc.read())[1].id == "n2"


def test_restore_empty_list_clears_notes():
    tc = _tc()
    _run(tc.post(_note("n1", "t1")))
    tc.restore([])
    assert _run(tc.read()) == []


# ---------------------------------------------------------------------------
# TaskCenter initialization
# ---------------------------------------------------------------------------


def test_task_center_stores_goal_and_user_request():
    tc = _tc(goal="build feature", user_request="please add auth")
    assert tc.goal == "build feature"
    assert tc.user_request == "please add auth"
