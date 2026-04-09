"""Tests for the ``share_briefing`` tool + ``team_context`` toolkit (Step 2g)."""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from team.artifacts.store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState
from team.runtime.registry import register, unregister
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata
from tools.team_context.share_briefing import share_briefing as _share_briefing_tool


async def _call(**kwargs):
    """Invoke the @tool-wrapped share_briefing through its execute() path."""
    context = kwargs.pop("context")
    args = _share_briefing_tool.input_model(**kwargs)
    return await _share_briefing_tool.execute(args, context)


share_briefing = _call  # tests below call ``share_briefing(...)`` directly.


def _fake_team_run(team_run_id: str = "T1", *, max_shared: int = 16) -> SimpleNamespace:
    budgets = BudgetConfig(max_shared_briefings=max_shared)
    state = BudgetState()
    store = InMemoryArtifactStore(budgets, state)
    return SimpleNamespace(
        id=team_run_id,
        budgets=budgets,
        artifacts=store,
        project_context=ProjectContext(goal="g", user_request="u"),
    )


def _ctx(team_run_id: str | None) -> ToolExecutionContext:
    meta = ExecutionMetadata(team_run_id=team_run_id or "")
    from pathlib import Path

    return ToolExecutionContext(cwd=Path("."), metadata=meta)


@pytest.mark.asyncio
async def test_share_briefing_promotes_inline():
    tr = _fake_team_run()
    register(tr)
    try:
        result = await _call(
            name="hint",
            source="inline",
            inline="remember the auth flow",
            description="why",
            context=_ctx("T1"),
        )
        assert not result.is_error
        assert "hint" in tr.project_context.shared_briefings
        b = tr.project_context.shared_briefings["hint"]
        assert b.inline == "remember the auth flow"
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_uses_canonical_scope_from_artifact():
    tr = _fake_team_run()
    tr.artifacts.save(
        "A1", {"summary": "scout", "target_paths": ["src/auth"], "canonical_scope": "src/auth"}
    )
    register(tr)
    try:
        result = await _call(
            name="auth_map", source="artifact", ref="A1", context=_ctx("T1")
        )
        assert not result.is_error
        assert "src/auth" in tr.project_context.shared_briefings
        assert "auth_map" not in tr.project_context.shared_briefings
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_derives_canonical_scope_from_target_paths():
    tr = _fake_team_run()
    tr.artifacts.save("A1", {"summary": "scout", "target_paths": ["src/foo/", "./src/bar"]})
    register(tr)
    try:
        result = await _call(
            name="foobar", source="artifact", ref="A1", context=_ctx("T1")
        )
        assert not result.is_error
        # Derived: sorted, deduped, slashes stripped
        assert "src/bar|src/foo" in tr.project_context.shared_briefings
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_falls_back_to_name_for_inline():
    tr = _fake_team_run()
    register(tr)
    try:
        await share_briefing(
            name="ad_hoc", source="inline", inline="x", context=_ctx("T1")
        )
        assert "ad_hoc" in tr.project_context.shared_briefings
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_replaces_on_collision():
    tr = _fake_team_run()
    register(tr)
    try:
        await share_briefing(
            name="dup", source="inline", inline="first", context=_ctx("T1")
        )
        result = await _call(
            name="dup", source="inline", inline="second", context=_ctx("T1")
        )
        assert not result.is_error
        assert "replaced=True" in result.output
        assert tr.project_context.shared_briefings["dup"].inline == "second"
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_enforces_cap():
    tr = _fake_team_run(max_shared=2)
    register(tr)
    try:
        await share_briefing(name="a", source="inline", inline="x", context=_ctx("T1"))
        await share_briefing(name="b", source="inline", inline="x", context=_ctx("T1"))
        result = await _call(
            name="c", source="inline", inline="x", context=_ctx("T1")
        )
        assert result.is_error
        assert "cap reached" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_validates_briefing_xor():
    tr = _fake_team_run()
    register(tr)
    try:
        result = await _call(
            name="bad", source="artifact", inline="oops", context=_ctx("T1")
        )
        assert result.is_error
        assert "invalid briefing" in result.output
        assert "source=\"inline\"" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_missing_inline_explains_literal_text_requirement():
    tr = _fake_team_run()
    register(tr)
    try:
        result = await _call(
            name="bad_inline",
            source="inline",
            context=_ctx("T1"),
        )
        assert result.is_error
        assert "literal non-empty" in result.output
        assert "skip promotion" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_missing_artifact_ref_explains_inline_fallback():
    tr = _fake_team_run()
    register(tr)
    try:
        result = await _call(
            name="bad_artifact",
            source="artifact",
            context=_ctx("T1"),
        )
        assert result.is_error
        assert "source=\"inline\"" in result.output
        assert "run_subagent" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_rejects_unknown_artifact_ref():
    tr = _fake_team_run()
    register(tr)
    try:
        result = await _call(
            name="missing",
            source="artifact",
            ref="ghost",
            context=_ctx("T1"),
        )
        assert result.is_error
        assert "unknown artifact ref" in result.output
        assert "source=\"inline\"" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_share_briefing_requires_team_run_id():
    result = await share_briefing(
        name="x", source="inline", inline="x", context=_ctx(None)
    )
    assert result.is_error
    assert "no team_run_id" in result.output


@pytest.mark.asyncio
async def test_share_briefing_unknown_team_run_id():
    result = await share_briefing(
        name="x", source="inline", inline="x", context=_ctx("ghost")
    )
    assert result.is_error
    assert "not registered" in result.output


def test_share_briefing_schema_advertises_source_specific_requirements():
    schema = _share_briefing_tool.to_api_schema()["input_schema"]
    assert schema["required"] == ["name", "source"]
    assert schema["oneOf"] == [
        {
            "title": "ArtifactBriefing",
            "properties": {"source": {"enum": ["artifact"]}},
            "required": ["ref"],
        },
        {
            "title": "InlineBriefing",
            "properties": {"source": {"enum": ["inline"]}},
            "required": ["inline"],
        },
    ]
