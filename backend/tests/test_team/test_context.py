"""Unit tests for team.context tiers."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from team.context.project import ProjectContext
from team.context.tools import build_team_context_tools
from team.types import WorkItem, WorkItemStatus


# ---- ProjectContext ----


def test_project_context_append_only():
    pc = ProjectContext(goal="g", user_request="u")
    pc.add_rationale("r1")
    pc.add_note("n1")
    pc.add_rationale("")  # ignored
    assert pc.rationale_history == ["r1"]
    assert pc.notes == ["n1"]
    assert pc.to_dict()["goal"] == "g"


@pytest.fixture(autouse=True)
def _agents_ok(monkeypatch):
    from team import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda n: True)


# ---- build_team_context_tools ----


@pytest.mark.asyncio
async def test_build_team_context_tools():
    from team.run import TeamRun

    tr = TeamRun(session_id="S1", user_request="req")
    wi = WorkItem(
        id="WIX",
        team_run_id=tr.id,
        agent_name="x",
        status=WorkItemStatus.RUNNING,
        started_at=datetime.now(timezone.utc) - timedelta(seconds=10),
    )
    tr.dispatcher.graph[wi.id] = wi

    tools = build_team_context_tools(tr, wi)
    names = {t.name for t in tools}
    assert names == {"team_get_project_context"}

    get_pc = next(t for t in tools if t.name == "team_get_project_context")
    pc = get_pc.callable()
    assert pc["user_request"] == "req"
