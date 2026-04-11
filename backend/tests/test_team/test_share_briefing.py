"""Tests for the ``share_briefing`` tool + ``team_context`` toolkit (Step 2g)."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

import pytest

from team.artifacts.store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.context.scout_briefings import (
    auto_promote_scout_briefing,
    invalidate_stale_scout_context,
)
from team.models import Briefing, BudgetConfig, BudgetState
from team.runtime.registry import register, unregister
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata
from tools.team_context.inspect_inherited_context import inspect_inherited_context
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


def _seed_context_pressure(tr: SimpleNamespace, scope: str) -> None:
    tr.project_context.scope_context_stats[scope] = {
        "lane_ids": {"developer-lane", "validator-lane"},
        "roles": {"developer", "validator"},
        "source_refs": {"payload:owned_files", "dep:auth-map"},
        "read_paths": {f"{scope}/service.py"},
        "verify_refs": {"tests/test_auth.py"},
        "failure_refs": set(),
        "developer_lane_ids": {"developer-lane"},
        "validator_after_developer": True,
    }


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
        assert tr.project_context.shared_briefing_meta["hint"]["kind"] == "runtime"
        assert tr.project_context.shared_briefing_meta["hint"]["provenance"] == "manual-inline"
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
async def test_share_briefing_evicts_auto_promoted_scout_when_full():
    tr = _fake_team_run(max_shared=1)
    _seed_context_pressure(tr, "src/auth")
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    assert auto_promote_scout_briefing(tr, "scout:src/auth")
    register(tr)
    try:
        result = await _call(
            name="manual", source="inline", inline="pin this", context=_ctx("T1")
        )
        assert not result.is_error
        assert "evicted_auto_promoted_scope='src/auth'" in result.output
        assert "manual" in tr.project_context.shared_briefings
        assert "src/auth" not in tr.project_context.shared_briefings
    finally:
        unregister("T1")


def test_auto_promote_scout_briefing_requires_same_run_context_pressure():
    tr = _fake_team_run()
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )

    assert not auto_promote_scout_briefing(tr, "scout:src/auth")
    assert tr.project_context.shared_briefings == {}


def test_auto_promote_scout_briefing_can_be_forced_for_root_scouts():
    tr = _fake_team_run()
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )

    assert auto_promote_scout_briefing(tr, "scout:src/auth", force=True)
    assert "src/auth" in tr.project_context.shared_briefings
    assert tr.project_context.shared_briefing_meta["src/auth"]["kind"] == "structural"
    assert tr.project_context.shared_briefing_meta["src/auth"]["provenance"] == "auto-scout"


def test_auto_promoted_eviction_clears_shared_meta():
    tr = _fake_team_run(max_shared=1)
    _seed_context_pressure(tr, "src/auth")
    _seed_context_pressure(tr, "src/payments")
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    tr.artifacts.save(
        "scout:src/payments",
        {
            "summary": "payments scout",
            "target_paths": ["src/payments"],
            "canonical_scope": "src/payments",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 110.0,
        },
    )

    assert auto_promote_scout_briefing(tr, "scout:src/auth")
    assert auto_promote_scout_briefing(tr, "scout:src/payments")
    assert "src/auth" not in tr.project_context.shared_briefings
    assert "src/auth" not in tr.project_context.shared_briefing_meta
    assert "src/payments" in tr.project_context.shared_briefings
    assert "src/payments" in tr.project_context.shared_briefing_meta


def test_auto_promote_scout_briefing_rejects_invalidated_scout_artifact():
    tr = _fake_team_run()
    tr.project_context.invalidated_scout_scopes["src/auth"] = 150.0
    _seed_context_pressure(tr, "src/auth")
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )

    assert not auto_promote_scout_briefing(tr, "scout:src/auth")
    assert tr.project_context.shared_briefings == {}


def test_share_briefing_rejects_stale_overlapping_scope_coherence(monkeypatch):
    tr = _fake_team_run()
    register(tr)
    try:
        ctx = _ctx("T1")
        ctx.metadata["scope_packet"] = {"scope_paths": ["src/auth"], "coherence_token": "old-token"}
        ctx.metadata["coherence_token"] = "old-token"
        monkeypatch.setattr(
            "tools.team_context.share_briefing.build_scope_packet_for_context",
            lambda context, scope_paths, baseline_packet=None: {
                "scope_paths": list(scope_paths or []),
                "coherence_token": "new-token",
                "freshness": "touched",
            },
        )

        result = asyncio.run(
            _call(
                name="src/auth",
                source="inline",
                inline="auth note",
                context=ctx,
            )
        )

        assert result.is_error
        assert "coherence changed" in result.output
        assert "src/auth" not in tr.project_context.shared_briefings
    finally:
        unregister("T1")


def test_inspect_inherited_context_returns_shared_entries_and_updates_coherence():
    tr = _fake_team_run()
    tr.project_context.shared_briefings["src/auth"] = Briefing(
        name="auth_note",
        source="inline",
        inline="remember the auth fallback",
        description="same-run note",
    )
    tr.project_context.shared_briefing_meta["src/auth"] = {
        "kind": "runtime",
        "provenance": "manual-inline",
        "stale_on_write": True,
        "scope_paths": ["src/auth"],
        "repo_epoch": 0,
        "scope_write_epoch": 0,
        "render_count": 2,
        "consumer_lane_ids": {"dev-a"},
        "consumer_roles": {"developer"},
        "source_coherence_token": "token-a",
        "source_packet_freshness": "fresh",
    }
    register(tr)
    try:
        ctx = _ctx("T1")
        result = asyncio.run(
            inspect_inherited_context.execute(
                inspect_inherited_context.input_model(
                    scope_paths=["src/auth/service.py"],
                    include_body=True,
                ),
                ctx,
            ),
        )

        assert not result.is_error
        data = json.loads(result.output)
        assert data["scope_paths"] == ["src/auth/service.py"]
        assert data["shared_context"][0]["scope"] == "src/auth"
        assert data["shared_context"][0]["source"] == "inline"
        assert data["shared_context"][0]["source_coherence_token"] == "token-a"
        assert "auth fallback" in data["shared_context"][0]["body_preview"]
        assert result.metadata["coherence_token"] == data["coherence_token"]
        assert ctx.metadata["coherence_token"] == data["coherence_token"]
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_explicit_scout_promotion_stays_pinned_against_later_auto_promotion():
    tr = _fake_team_run(max_shared=1)
    _seed_context_pressure(tr, "src/auth")
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    tr.artifacts.save(
        "scout:src/payments",
        {
            "summary": "payments scout",
            "target_paths": ["src/payments"],
            "canonical_scope": "src/payments",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 110.0,
        },
    )
    assert auto_promote_scout_briefing(tr, "scout:src/auth")
    register(tr)
    try:
        result = await _call(
            name="auth_map", source="artifact", ref="scout:src/auth", context=_ctx("T1")
        )
        assert not result.is_error
        assert "src/auth" not in tr.project_context.auto_promoted_scout_scopes
        assert not auto_promote_scout_briefing(tr, "scout:src/payments")
        assert sorted(tr.project_context.shared_briefings.keys()) == ["src/auth"]
    finally:
        unregister("T1")


def test_invalidate_stale_scout_context_removes_overlapping_scout_briefings_and_versions():
    tr = _fake_team_run()
    tr.project_context.repo_root = "/repo"
    _seed_context_pressure(tr, "src/auth")
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    assert auto_promote_scout_briefing(tr, "scout:src/auth")
    tr.project_context.stable_scout_versions["src/auth"] = {
        "snapshot_time": 100.0,
        "run_id": "run-a",
    }
    tr.project_context.shared_briefings["manual"] = Briefing(
        name="manual",
        source="inline",
        inline="keep this note",
    )

    invalidated = invalidate_stale_scout_context(tr, "/repo/src/auth/service.py")

    assert invalidated == ["src/auth"]
    assert tr.project_context.repo_epoch == 1
    assert tr.project_context.scope_write_epochs["src/auth"] == 1
    assert "src/auth" not in tr.project_context.shared_briefings
    assert "src/auth" not in tr.project_context.shared_briefing_meta
    assert "src/auth" not in tr.project_context.stable_scout_versions
    assert "src/auth" not in tr.project_context.auto_promoted_scout_scopes
    assert "manual" in tr.project_context.shared_briefings


def test_invalidate_stale_scout_context_matches_compound_scope_members():
    tr = _fake_team_run()
    tr.project_context.repo_root = "/repo"
    tr.artifacts.save(
        "scout:src/bar|src/foo",
        {
            "summary": "compound scout",
            "target_paths": ["src/foo", "src/bar"],
            "canonical_scope": "src/bar|src/foo",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    tr.project_context.shared_briefings["src/bar|src/foo"] = Briefing(
        name="compound",
        source="artifact",
        ref="scout:src/bar|src/foo",
    )
    tr.project_context.stable_scout_versions["src/bar|src/foo"] = {
        "snapshot_time": 100.0,
        "run_id": "run-b",
    }

    invalidated = invalidate_stale_scout_context(tr, "src/foo/service.py")

    assert invalidated == ["src/bar|src/foo"]
    assert "src/bar|src/foo" not in tr.project_context.shared_briefings
    assert "src/bar|src/foo" not in tr.project_context.stable_scout_versions


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
async def test_share_briefing_rejects_invalidated_scout_artifact_ref():
    tr = _fake_team_run()
    tr.project_context.invalidated_scout_scopes["src/auth"] = 150.0
    tr.artifacts.save(
        "scout:src/auth",
        {
            "summary": "auth scout",
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    register(tr)
    try:
        result = await _call(
            name="auth_map",
            source="artifact",
            ref="scout:src/auth",
            context=_ctx("T1"),
        )
        assert result.is_error
        assert "predates a same-run overlapping edit" in result.output
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
