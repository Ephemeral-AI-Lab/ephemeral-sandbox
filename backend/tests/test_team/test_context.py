"""Unit tests for team.context tiers."""

from __future__ import annotations

from datetime import datetime, timedelta

import pytest

from team.artifact_store import InMemoryArtifactStore
from team.context.files import (
    ChangeLog,
    ChangeLogEntry,
    get_active_team_run,
    record_file_edit_from_hook_payload,
    register_team_run,
    unregister_team_run,
)
from team.context.project import ProjectContext
from team.context.siblings import SiblingView
from team.context.tools import build_team_context_tools
from team.dispatcher import Dispatcher
from team.types import BudgetConfig, BudgetState, WorkItem, WorkItemStatus


# ---- ProjectContext ----


def test_project_context_append_only():
    pc = ProjectContext(goal="g", user_request="u")
    pc.add_rationale("r1")
    pc.add_note("n1")
    pc.add_rationale("")  # ignored
    assert pc.rationale_history == ["r1"]
    assert pc.notes == ["n1"]
    assert pc.to_dict()["goal"] == "g"


# ---- SiblingView ----


def _make_disp():
    b = BudgetConfig()
    s = BudgetState()
    store = InMemoryArtifactStore(b, s)
    return Dispatcher("T1", b, s, store), store


@pytest.fixture(autouse=True)
def _agents_ok(monkeypatch):
    from team import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda n: True)


@pytest.mark.asyncio
async def test_sibling_view_live_updates():
    disp, store = _make_disp()
    a = WorkItem(id="A", team_run_id="T1", agent_name="x", status=WorkItemStatus.PENDING)
    b = WorkItem(id="B", team_run_id="T1", agent_name="y", status=WorkItemStatus.PENDING)
    await disp.add_work_item(a)
    await disp.add_work_item(b)
    view = SiblingView(disp, "A", store)
    siblings = view.list()
    assert len(siblings) == 1
    assert siblings[0].work_item_id == "B"
    assert siblings[0].status == "ready"

    # complete B and observe via the live view
    from team.types import AgentResult

    await disp.pop_ready()
    await disp.pop_ready()
    await disp.mark_running("B", "AR1")
    await disp.complete("B", AgentResult(artifact={"summary": "done"}, summary="done"))
    live = view.list()
    assert live[0].status == "done"
    assert "done" in live[0].artifact_summary

    # status filter
    assert view.list(status="done")[0].work_item_id == "B"
    assert view.list(status="running") == []


# ---- ChangeLog ----


def test_changelog_since_filters_by_timestamp_and_self():
    cl = ChangeLog()
    t0 = datetime.utcnow()
    cl.append(ChangeLogEntry(work_item_id="W1", agent_run_id="A1", filepath="/a.py"))
    cl.append(ChangeLogEntry(work_item_id="W2", agent_run_id="A2", filepath="/b.py"))

    after_t0 = cl.since(t0 - timedelta(seconds=1))
    assert len(after_t0) == 2

    future = cl.since(datetime.utcnow() + timedelta(seconds=1))
    assert future == []

    excluded = cl.since(None, exclude_work_item_id="W1")
    assert [e.work_item_id for e in excluded] == ["W2"]


def test_changelog_restore_replaces_entries():
    cl = ChangeLog()
    cl.append(ChangeLogEntry(work_item_id="W1", agent_run_id=None, filepath="/x"))
    snap = cl.all()
    cl.append(ChangeLogEntry(work_item_id="W2", agent_run_id=None, filepath="/y"))
    cl.restore(snap)
    assert [e.work_item_id for e in cl.all()] == ["W1"]


# ---- Hook subscriber routing ----


class _FakeTeamRun:
    def __init__(self, id_: str) -> None:
        self.id = id_
        self.change_log = ChangeLog()


def test_hook_subscriber_routes_to_active_team_run():
    tr = _FakeTeamRun("T-HOOK")
    register_team_run(tr)  # type: ignore[arg-type]
    try:
        recorded = record_file_edit_from_hook_payload(
            {
                "tool_name": "str_replace_based_edit_tool",
                "tool_input": {"path": "/tmp/f.py"},
                "team_context": {
                    "team_run_id": "T-HOOK",
                    "work_item_id": "W1",
                    "agent_run_id": "A1",
                },
            }
        )
        assert recorded is True
        entries = tr.change_log.all()
        assert len(entries) == 1
        assert entries[0].filepath == "/tmp/f.py"
    finally:
        unregister_team_run("T-HOOK")
    assert get_active_team_run("T-HOOK") is None


def test_hook_subscriber_ignores_non_file_tool():
    assert (
        record_file_edit_from_hook_payload(
            {
                "tool_name": "shell_exec",
                "tool_input": {},
                "team_context": {"team_run_id": "T1"},
            }
        )
        is False
    )


def test_hook_subscriber_ignores_events_without_team_context():
    assert (
        record_file_edit_from_hook_payload(
            {
                "tool_name": "str_replace_based_edit_tool",
                "tool_input": {"path": "/x"},
            }
        )
        is False
    )


# ---- build_team_context_tools ----


@pytest.mark.asyncio
async def test_build_team_context_tools(monkeypatch):
    from team.run import TeamRun

    tr = TeamRun(session_id="S1", user_request="req")
    wi = WorkItem(
        id="WIX",
        team_run_id=tr.id,
        agent_name="x",
        status=WorkItemStatus.RUNNING,
        started_at=datetime.utcnow() - timedelta(seconds=10),
    )
    tr.dispatcher.graph[wi.id] = wi
    tr.change_log.append(
        ChangeLogEntry(work_item_id="OTHER", agent_run_id="A1", filepath="/x.py")
    )

    tools = build_team_context_tools(tr, wi)
    names = {t.name for t in tools}
    assert names == {
        "team_get_project_context",
        "team_list_siblings",
        "team_files_changed_since_dispatch",
    }

    get_pc = next(t for t in tools if t.name == "team_get_project_context")
    pc = get_pc.callable()
    assert pc["user_request"] == "req"

    files = next(t for t in tools if t.name == "team_files_changed_since_dispatch")
    assert files.callable()[0]["filepath"] == "/x.py"
