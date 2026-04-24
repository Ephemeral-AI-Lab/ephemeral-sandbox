"""Unit tests for team.task_center.TaskCenter."""

from __future__ import annotations

import asyncio
import time

from team.core.models import BudgetConfig, BudgetState, Note, Task, TaskDefinition, TaskStatus
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
    content: str = "some content",
    *,
    agent_name: str = "developer",
    timestamp: float | None = None,
    paths: list[str] | None = None,
) -> Note:
    return Note(
        id=id_,
        agent_name=agent_name,
        content=content,
        timestamp=timestamp if timestamp is not None else time.time(),
        paths=paths or [],
    )


def _task(
    id_: str,
    goal: str = "do work",
    deps: list[str] | None = None,
    scope_paths: list[str] | None = None,
    parent_id: str | None = None,
) -> Task:
    return Task(
        id=id_,
        team_run_id="run-1",
        definition=TaskDefinition(
            id=id_,
            spec={
                "goal": goal,
                "detail": f"Detail for {goal}",
                "acceptance_criteria": f"Acceptance for {goal}",
            },
            agent="developer",
            deps=deps or [],
            scope_paths=scope_paths or [],
        ),
        status=TaskStatus.PENDING,
        parent_id=parent_id,
    )


def _run(awaitable):
    return asyncio.run(awaitable)


# ---------------------------------------------------------------------------
# Basic post / read
# ---------------------------------------------------------------------------


def test_empty_task_center_returns_empty_reads():
    tc = _tc()
    assert _run(tc.notes.read()) == []


def test_post_appends_notes():
    tc = _tc()
    n1 = _note("n1", "hello")
    n2 = _note("n2", "world")
    _run(tc.notes.post(n1))
    _run(tc.notes.post(n2))
    notes = _run(tc.notes.read())
    assert len(notes) == 2
    assert notes[0].id == "n1"
    assert notes[1].id == "n2"


def test_post_emits_note_posted_event():
    store = _RecordingStore()
    tc = _tc(event_store=store)

    _run(
        tc.notes.post(
            _note(
                "n1",
                "first line\nsecond line",
                agent_name="developer (auto)",
                paths=["src/auth"],
            )
        )
    )

    assert len(store.events) == 1
    event = store.events[0]
    assert event.kind == "note_posted"
    assert event.data["agent_name"] == "developer (auto)"
    assert event.data["scope_paths"] == ["src/auth"]
    assert event.data["content_preview"] == "first line second line"


def test_post_logs_file_scoped_note(caplog):
    tc = _tc()

    with caplog.at_level("INFO", logger="team.task_center"):
        _run(
            tc.notes.post(
                _note("n1", "checkpoint summary", agent_name="developer (auto)")
            )
        )

    assert "[task_center] note agent=developer (auto)" in caplog.text


# ---------------------------------------------------------------------------
# Filtering: scope_paths (prefix matching)
# ---------------------------------------------------------------------------


def test_read_paths_prefix_match():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/auth/session.py"])))
    _run(tc.notes.post(_note("n2", paths=["src/billing/invoice.py"])))

    results = _run(tc.notes.read(paths=["src/auth"]))
    assert len(results) == 1
    assert results[0].id == "n1"


def test_read_paths_exact_match():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/auth"])))
    results = _run(tc.notes.read(paths=["src/auth"]))
    assert len(results) == 1


def test_read_paths_no_paths_on_note_excludes_note():
    tc = _tc()
    _run(tc.notes.post(_note("n1")))
    results = _run(tc.notes.read(paths=["src/auth"]))
    assert results == []


def test_read_paths_trailing_slash_stripped():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/auth/session.py"])))
    results = _run(tc.notes.read(paths=["src/auth/"]))
    assert len(results) == 1


def test_read_paths_matches_broader_note_paths_from_narrow_query():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/auth"])))
    results = _run(tc.notes.read(paths=["src/auth/session.py"]))
    assert len(results) == 1
    assert results[0].id == "n1"


def test_read_paths_respects_component_boundaries():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/authz.py"])))
    assert _run(tc.notes.read(paths=["src/auth"])) == []


# ---------------------------------------------------------------------------
# Filtering: since
# ---------------------------------------------------------------------------


def test_read_since_filters_by_timestamp():
    tc = _tc()
    _run(tc.notes.post(_note("n1", timestamp=100.0)))
    _run(tc.notes.post(_note("n2", timestamp=200.0)))
    _run(tc.notes.post(_note("n3", timestamp=300.0)))

    results = _run(tc.notes.read(since=200.0))
    assert len(results) == 2
    assert {n.id for n in results} == {"n2", "n3"}


def test_read_since_none_returns_all():
    tc = _tc()
    _run(tc.notes.post(_note("n1", timestamp=100.0)))
    _run(tc.notes.post(_note("n2", timestamp=200.0)))
    assert len(_run(tc.notes.read(since=None))) == 2


# ---------------------------------------------------------------------------
# Filtering: limit
# ---------------------------------------------------------------------------


def test_read_limit_returns_last_n():
    tc = _tc()
    for i in range(5):
        _run(tc.notes.post(_note(f"n{i}")))

    results = _run(tc.notes.read(last_n=3))
    assert len(results) == 3
    assert results[0].id == "n2"
    assert results[-1].id == "n4"


def test_read_limit_larger_than_total_returns_all():
    tc = _tc()
    _run(tc.notes.post(_note("n1")))
    results = _run(tc.notes.read(last_n=100))
    assert len(results) == 1


# ---------------------------------------------------------------------------
# Combined filters
# ---------------------------------------------------------------------------


def test_read_combined_paths_and_since():
    tc = _tc()
    _run(tc.notes.post(_note("n1", timestamp=100.0, paths=["src/auth/a.py"])))
    _run(tc.notes.post(_note("n2", timestamp=300.0, paths=["src/auth/b.py"])))
    _run(tc.notes.post(_note("n3", timestamp=300.0, paths=["src/billing/a.py"])))

    results = _run(tc.notes.read(paths=["src/auth"], since=200.0))
    assert len(results) == 1
    assert results[0].id == "n2"


def test_read_combined_paths_and_last_n():
    tc = _tc()
    _run(tc.notes.post(_note("n1", paths=["src/auth/a.py"])))
    _run(tc.notes.post(_note("n2", paths=["src/auth/b.py"])))
    _run(tc.notes.post(_note("n3", paths=["src/auth/c.py"])))

    results = _run(tc.notes.read(paths=["src/auth"], last_n=2))
    assert len(results) == 2
    assert results[0].id == "n2"
    assert results[1].id == "n3"


# ---------------------------------------------------------------------------
# context_for
# ---------------------------------------------------------------------------


def test_context_for_always_includes_task_section():
    tc = _tc()
    task = _task("work-1", goal="implement login flow")
    ctx = _run(tc.context.context_for(task))
    assert "## Your task" in ctx
    assert "implement login flow" in ctx


def test_context_for_includes_scope_paths_when_present():
    tc = _tc()
    task = _task("work-1", goal="do auth", scope_paths=["src/auth/"])
    ctx = _run(tc.context.context_for(task))
    assert "Scope:" in ctx
    assert "src/auth/" in ctx


def test_context_for_no_scope_paths_omits_scope_line():
    tc = _tc()
    task = _task("work-1", goal="general work")
    ctx = _run(tc.context.context_for(task))
    assert "Scope:" not in ctx


def test_context_for_does_not_include_dep_notes():
    tc = _tc()
    _run(tc.notes.post(_note("n1", "dependency output", agent_name="developer")))
    task = _task("work-1", goal="build on dep", deps=["dep-task"])
    ctx = _run(tc.context.context_for(task))
    assert "Context from dependencies" not in ctx
    assert "dependency output" not in ctx


def test_context_for_dep_notes_absent_when_no_deps():
    tc = _tc()
    _run(tc.notes.post(_note("n1", "some output")))
    task = _task("work-1", goal="standalone work")
    ctx = _run(tc.context.context_for(task))
    assert "Context from dependencies" not in ctx


def test_context_for_does_not_include_parent_notes_when_parent_id_matches():
    tc = _tc()
    _run(tc.notes.post(_note("n1", "parent reasoning", agent_name="team_planner")))
    task = _task("work-1", goal="child task", parent_id="parent-task")

    # Mock get_task so _parent_chain_ids doesn't hit DB
    parent = _task("parent-task", goal="parent")

    async def _mock_get_task(task_id):
        return parent if task_id == "parent-task" else None

    tc.get_task = _mock_get_task

    ctx = _run(tc.context.context_for(task))
    assert "Parent context" not in ctx
    assert "parent reasoning" not in ctx


def test_context_for_does_not_walk_parent_chain_for_notes():
    tc = _tc()
    _run(tc.notes.post(_note("n1", "root rationale", agent_name="team_planner")))
    _run(tc.notes.post(_note("n2", "parent reasoning", agent_name="team_planner")))
    task = _task("work-1", goal="child task", parent_id="parent-task")

    # Mock get_task to simulate parent chain without DB
    parent = _task("parent-task", goal="parent", parent_id="root-task")
    root = _task("root-task", goal="root")

    async def _mock_get_task(task_id):
        if task_id == "parent-task":
            return parent
        if task_id == "root-task":
            return root
        return None

    tc.get_task = _mock_get_task

    ctx = _run(tc.context.context_for(task))
    assert "root rationale" not in ctx
    assert "parent reasoning" not in ctx


def test_context_for_ignores_parent_notes():
    tc = _tc()
    _run(
        tc.notes.post(
            _note(
                "n1",
                "stale parent note",
                agent_name="team_planner (auto)",
                timestamp=100.0,
            )
        )
    )
    _run(
        tc.notes.post(
            _note(
                "n2",
                "fresh parent note",
                agent_name="team_planner (auto)",
                timestamp=200.0,
            )
        )
    )
    task = _task("work-1", goal="child task", parent_id="parent-task")

    parent = _task("parent-task", goal="parent")

    async def _mock_get_task(task_id):
        return parent if task_id == "parent-task" else None

    tc.get_task = _mock_get_task

    ctx = _run(tc.context.context_for(task))
    assert "fresh parent note" not in ctx
    assert "stale parent note" not in ctx


def test_context_for_no_parent_notes_when_parent_id_is_none():
    tc = _tc()
    _run(tc.notes.post(_note("n1", "context")))
    task = _task("work-1", goal="root level task")
    ctx = _run(tc.context.context_for(task))
    assert "Parent context" not in ctx


def test_context_for_respects_max_context_bytes():
    tc = _tc()
    big_content = "x" * 100_000
    _run(tc.notes.post(_note("n1", big_content, agent_name="developer")))
    task = _task("work-1", goal="build on dep", deps=["dep-task"])

    ctx = _run(tc.context.context_for(task, max_context_bytes=500))
    assert "## Your task" in ctx
    assert len(ctx.encode()) < 100_000


def test_context_for_task_section_never_trimmed():
    tc = _tc()
    big_content = "z" * 200_000
    _run(tc.notes.post(_note("n1", big_content)))
    task = _task("work-1", goal="important task description", deps=["dep-task"])
    ctx = _run(tc.context.context_for(task, max_context_bytes=100))
    assert "important task description" in ctx


# ---------------------------------------------------------------------------
# TaskCenter initialization
# ---------------------------------------------------------------------------
