"""Tests for PostgreSQL infrastructure components.

Tests ltree_utils, ORM models, and DispatcherStore structure.
Integration tests with a real PG instance are separate — these run
without a database.
"""

from __future__ import annotations

import asyncio
import re
from types import SimpleNamespace

from sqlalchemy import Text

from team.models import BudgetConfig, BudgetState, TERMINAL_STATUSES, Task, TaskStatus
from team.persistence.ltree_utils import _escape_char, path_to_ltree
from team.persistence import task_queries
from team.persistence.task_record import TaskRecord
from team.runtime.task_queue import TaskQueue
from team.task_center import TaskCenter

# ---------------------------------------------------------------------------
# ltree_utils
# ---------------------------------------------------------------------------


class TestPathToLtree:
    def test_simple_directory(self):
        assert path_to_ltree("src/auth/") == "src.auth"

    def test_file_with_extension(self):
        assert path_to_ltree("src/auth/session.py") == "src.auth.sessionDpy"

    def test_init_file(self):
        assert path_to_ltree("src/auth/__init__.py") == "src.auth.__init__Dpy"

    def test_dotted_filename(self):
        assert path_to_ltree("src/payment/utils.v2.py") == "src.payment.utilsDv2Dpy"

    def test_hyphenated_module(self):
        assert path_to_ltree("src/my-module/foo.py") == "src.myHmodule.fooDpy"

    def test_underscore_module(self):
        assert path_to_ltree("src/my_module/foo.py") == "src.my_module.fooDpy"

    def test_leading_slash_stripped(self):
        assert path_to_ltree("/leading/slash") == "leading.slash"

    def test_trailing_slash_stripped(self):
        assert path_to_ltree("trailing/slash/") == "trailing.slash"

    def test_no_collision_hyphen_vs_underscore(self):
        """Hyphen and underscore must produce different labels."""
        assert path_to_ltree("my-mod") != path_to_ltree("my_mod")

    def test_empty_components_dropped(self):
        assert path_to_ltree("a//b") == "a.b"

    def test_labels_are_ltree_safe(self):
        """All labels must match [a-zA-Z0-9_]+."""
        result = path_to_ltree("src/some.weird-file@v2.py")
        for label in result.split("."):
            assert re.match(r"^[a-zA-Z0-9_]+$", label), f"Unsafe label: {label}"


class TestEscapeChar:
    def test_dot(self):
        assert _escape_char(".") == "D"

    def test_hyphen(self):
        assert _escape_char("-") == "H"

    def test_at_sign(self):
        assert _escape_char("@") == "X40"

    def test_space(self):
        assert _escape_char(" ") == "X20"


# ---------------------------------------------------------------------------
# ORM models — structure checks
# ---------------------------------------------------------------------------


class TestTaskRecord:
    def test_tablename(self):
        assert TaskRecord.__tablename__ == "tasks"

    def test_composite_pk(self):
        pk_cols = {c.name for c in TaskRecord.__table__.primary_key.columns}
        assert pk_cols == {"id", "team_run_id"}

    def test_explicit_status(self):
        r = TaskRecord(
            id="t1", team_run_id="r1", agent_name="dev", objective="do stuff", status="pending"
        )
        assert r.status == "pending"

    def test_explicit_deps(self):
        r = TaskRecord(id="t1", team_run_id="r1", agent_name="dev", objective="x", deps=["a"])
        assert r.deps == ["a"]

    def test_status_column_is_unbounded_text(self):
        assert isinstance(TaskRecord.status.type, Text)
        assert TaskRecord.status.type.length is None
        assert max(len(status.value) for status in TaskStatus) > 16

# ---------------------------------------------------------------------------
# TaskCenter — structure check (no DB)
# ---------------------------------------------------------------------------


class TestTaskCenterStructure:
    def test_has_required_methods(self):
        """Facade surface: add_task + manager property accessors."""
        assert callable(getattr(TaskCenter, "add_task", None))
        assert callable(getattr(TaskCenter, "get_task", None))
        assert callable(getattr(TaskCenter, "emit_event", None))
        # Manager accessors exposed as properties
        assert isinstance(getattr(TaskCenter, "notes", None), property)
        assert isinstance(getattr(TaskCenter, "store", None), property)
        assert isinstance(getattr(TaskCenter, "budget", None), property)
        assert isinstance(getattr(TaskCenter, "expander", None), property)
        assert isinstance(getattr(TaskCenter, "context", None), property)


class TestTaskQueueStructure:
    def test_has_required_surface(self):
        assert callable(getattr(TaskQueue, "enqueue", None))
        assert callable(getattr(TaskQueue, "start", None))
        assert callable(getattr(TaskQueue, "drain_and_stop", None))


class _FakeCascadeResult:
    def __init__(self, rows=None) -> None:
        self._rows = rows or []

    def scalars(self):
        return self

    def all(self):
        return self._rows

    def fetchall(self):
        return []


class _FakeCascadeSession:
    def __init__(self) -> None:
        self.statements: list[str] = []

    async def execute(self, statement, *args, **kwargs):
        del args, kwargs
        self.statements.append(str(statement))
        return _FakeCascadeResult()

    async def commit(self) -> None:
        return None


class _FakeSessionFactory:
    def __init__(self, session: _FakeCascadeSession) -> None:
        self._session = session

    def __call__(self):
        session = self._session

        class _Ctx:
            async def __aenter__(self_inner):
                return session

            async def __aexit__(self_inner, exc_type, exc, tb):
                del exc_type, exc, tb
                return False

        return _Ctx()


class _FakeCountResult:
    def scalar(self):
        return 0


class _FakeCountSession:
    def __init__(self) -> None:
        self.statement = None

    async def execute(self, statement, *args, **kwargs):
        del args, kwargs
        self.statement = statement
        return _FakeCountResult()


def test_count_non_terminal_excludes_all_terminal_statuses():
    session = _FakeCountSession()

    asyncio.run(task_queries.count_non_terminal(session, "run-1"))

    params = session.statement.compile().params
    assert set(params["status_1"]) == {status.value for status in TERMINAL_STATUSES}


def test_cascade_cancel_recursive_loads_non_terminal_task_graph():
    session = _FakeCascadeSession()
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(session),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )

    asyncio.run(tc.store.cascade_cancel_recursive("task-1"))

    sql = session.statements[0]
    assert "SELECT tasks.id" in sql
    assert "tasks.status NOT IN" in sql


def test_all_detached_expandable_parent_awaits_summary_instead_of_failing(
    monkeypatch,
):
    session = _FakeCascadeSession()
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(session),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    parent = Task(
        id="parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED,
        objective="parent",
    )
    child = Task(
        id="child",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.CANCELLED,
        objective="child",
        parent_id="parent",
    )
    tc.store.graph = {"parent": parent, "child": child}
    marked_awaiting: list[str] = []

    async def _fake_parent_candidate(db, team_run_id, current_id):
        del db, team_run_id
        if current_id == "child":
            return SimpleNamespace(id="parent", all_detached=True)
        return None

    async def _mark_awaiting(task_id: str) -> None:
        marked_awaiting.append(task_id)
        parent.status = TaskStatus.EXPANDED_AWAITING_SUMMARY

    async def _unexpected_failed(task_id: str, status: str, reason: str) -> None:
        raise AssertionError(f"detached parent must not be marked {status}: {task_id} {reason}")

    monkeypatch.setattr(
        task_queries,
        "fetch_expanded_parent_candidate",
        _fake_parent_candidate,
    )
    monkeypatch.setattr(tc.store, "mark_expanded_awaiting_summary", _mark_awaiting)
    monkeypatch.setattr(tc.store, "mark_terminal", _unexpected_failed)

    promoted, awaiting = asyncio.run(tc.store.maybe_promote_expanded_parent("child"))

    assert promoted == []
    assert awaiting == ["parent"]
    assert marked_awaiting == ["parent"]
    assert parent.status == TaskStatus.EXPANDED_AWAITING_SUMMARY
