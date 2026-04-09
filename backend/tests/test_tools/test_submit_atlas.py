"""Unit tests for ``SubmitAtlasTool`` (Phase 2 Step 9)."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from db.base import Base
from team.artifacts.store import InMemoryArtifactStore
from team.atlas import AtlasStore
from team.atlas.model import ProjectAtlasChunkRecord, ProjectAtlasRecord  # noqa: F401
from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState
from team.runtime.registry import register, unregister
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata
from tools.posthook.submit_atlas import SubmitAtlasInput, SubmitAtlasTool


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def atlas_store() -> AtlasStore:
    engine = create_engine("sqlite:///:memory:", echo=False)
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    store = AtlasStore()
    store.initialize(factory)
    return store


def _fake_team_run(
    team_run_id: str,
    *,
    project_key: str = "P1",
    repo_root: str = "/repo",
) -> SimpleNamespace:
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    project_ctx = ProjectContext(
        goal="g",
        user_request="u",
        project_key=project_key,
        repo_root=repo_root,
    )
    return SimpleNamespace(
        id=team_run_id,
        budgets=budgets,
        artifacts=artifacts,
        project_context=project_ctx,
    )


def _ctx(
    team_run_id: str | None,
    *,
    atlas_store: AtlasStore | None = None,
) -> ToolExecutionContext:
    meta = ExecutionMetadata(team_run_id=team_run_id or "")
    if atlas_store is not None:
        meta.extras["atlas_store"] = atlas_store
    return ToolExecutionContext(cwd=Path("."), metadata=meta)


def _scout_brief(paths: list[str]) -> dict:
    return {
        "target_paths": paths,
        "canonical_scope": "|".join(sorted(paths)),
        "summary": f"brief for {paths}",
        "files": [],
        "entry_points": [],
        "open_questions": [],
        "scope_coverage": 1.0,
        "gaps": "",
        "suggested_subdivisions": [],
    }


async def _run(
    tool: SubmitAtlasTool, **kwargs
) -> tuple:
    ctx = kwargs.pop("context")
    args = SubmitAtlasInput(**kwargs)
    result = await tool.execute(args, ctx)
    return result, ctx.metadata.get("submitted_atlas")


def test_submit_atlas_input_accepts_json_string_chunks() -> None:
    args = SubmitAtlasInput.model_validate(
        {"chunks": json.dumps([{"brief": _scout_brief(["src/a"])}])}
    )
    assert len(args.chunks) == 1
    assert args.chunks[0].brief["target_paths"] == ["src/a"]


# ---------------------------------------------------------------------------
# Happy path
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_submit_atlas_writes_chunks_and_stashes_summary(
    atlas_store: AtlasStore,
) -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1", repo_root="/tmp/somewhere")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=atlas_store)
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        args = SubmitAtlasInput(
            chunks=[
                {"brief": _scout_brief(["src/a"])},
                {"subsystem": "src/b", "brief": _scout_brief(["src/b"])},
            ],
            rationale="first pass",
        )
        result = await tool.execute(args, ctx)

        assert not result.is_error, result.output
        payload = ctx.metadata.get("submitted_atlas")
        assert payload is not None
        assert "Atlas updated" in payload.summary
        assert payload.artifact["subsystems"] == ["src/a", "src/b"]
        assert payload.artifact["project_key"] == "P1"

        # Chunks are durable.
        a = atlas_store.get_chunk("P1", "src/a")
        b = atlas_store.get_chunk("P1", "src/b")
        assert a is not None and a.brief["target_paths"] == ["src/a"]
        assert b is not None and b.brief["target_paths"] == ["src/b"]
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_submit_atlas_derives_subsystem_from_canonical_scope(
    atlas_store: AtlasStore,
) -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=atlas_store)
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        brief = _scout_brief(["src/foo", "src/bar"])
        args = SubmitAtlasInput(chunks=[{"brief": brief}])
        result = await tool.execute(args, ctx)
        assert not result.is_error, result.output

        # canonical_scope sorted: src/bar|src/foo
        chunk = atlas_store.get_chunk("P1", "src/bar|src/foo")
        assert chunk is not None
    finally:
        unregister("T1")


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_submit_atlas_requires_team_run_id() -> None:
    tool = SubmitAtlasTool()
    ctx = _ctx(None)
    ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
    result = await tool.execute(
        SubmitAtlasInput(chunks=[{"brief": _scout_brief(["x"])}]), ctx
    )
    assert result.is_error
    assert "team_run_id" in result.output


@pytest.mark.asyncio
async def test_submit_atlas_unknown_team_run_id() -> None:
    tool = SubmitAtlasTool()
    ctx = _ctx("ghost")
    ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
    result = await tool.execute(
        SubmitAtlasInput(chunks=[{"brief": _scout_brief(["x"])}]), ctx
    )
    assert result.is_error
    assert "not registered" in result.output


@pytest.mark.asyncio
async def test_submit_atlas_rejects_missing_project_key() -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1", project_key="", repo_root="")
    register(tr)
    try:
        ctx = _ctx("T1")
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        result = await tool.execute(
            SubmitAtlasInput(chunks=[{"brief": _scout_brief(["x"])}]), ctx
        )
        assert result.is_error
        assert "atlas is disabled" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_submit_atlas_rejects_chunk_without_resolvable_subsystem(
    atlas_store: AtlasStore,
) -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=atlas_store)
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        args = SubmitAtlasInput(chunks=[{"brief": {"target_paths": []}}])
        result = await tool.execute(args, ctx)
        assert result.is_error
        assert "missing subsystem" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_submit_atlas_rejects_duplicate_subsystem(
    atlas_store: AtlasStore,
) -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=atlas_store)
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        args = SubmitAtlasInput(
            chunks=[
                {"subsystem": "src/a", "brief": _scout_brief(["src/a"])},
                {"subsystem": "src/a", "brief": _scout_brief(["src/a"])},
            ]
        )
        result = await tool.execute(args, ctx)
        assert result.is_error
        assert "duplicate subsystem" in result.output
    finally:
        unregister("T1")


@pytest.mark.asyncio
async def test_submit_atlas_uninitialised_store() -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=AtlasStore())  # not initialised
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        args = SubmitAtlasInput(chunks=[{"brief": _scout_brief(["src/a"])}])
        result = await tool.execute(args, ctx)
        assert result.is_error
        assert "not initialised" in result.output
    finally:
        unregister("T1")


# ---------------------------------------------------------------------------
# Refresher semantics — upsert of existing chunks
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_submit_atlas_refresher_upserts_existing_chunks(
    atlas_store: AtlasStore,
) -> None:
    tool = SubmitAtlasTool()
    tr = _fake_team_run("T1")
    register(tr)
    try:
        ctx = _ctx("T1", atlas_store=atlas_store)
        ctx.metadata["posthook_metadata_key"] = "submitted_atlas"
        await tool.execute(
            SubmitAtlasInput(
                chunks=[
                    {"subsystem": "src/a", "brief": _scout_brief(["src/a"])},
                    {"subsystem": "src/b", "brief": _scout_brief(["src/b"])},
                ]
            ),
            ctx,
        )

        # Refresh only src/a.
        ctx2 = _ctx("T1", atlas_store=atlas_store)
        ctx2.metadata["posthook_metadata_key"] = "submitted_atlas"
        fresh_brief = {**_scout_brief(["src/a"]), "summary": "refreshed"}
        result = await tool.execute(
            SubmitAtlasInput(
                chunks=[{"subsystem": "src/a", "brief": fresh_brief}]
            ),
            ctx2,
        )
        assert not result.is_error, result.output

        a = atlas_store.get_chunk("P1", "src/a")
        b = atlas_store.get_chunk("P1", "src/b")
        assert a is not None and a.brief["summary"] == "refreshed"
        # src/b untouched.
        assert b is not None and "brief for" in b.brief["summary"]
    finally:
        unregister("T1")
