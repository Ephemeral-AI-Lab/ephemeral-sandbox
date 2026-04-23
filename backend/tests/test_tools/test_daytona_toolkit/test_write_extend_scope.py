"""Tests for the write-scope-extension post-hook."""

from __future__ import annotations

import asyncio
from pathlib import Path

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import PostHookOutcome, ToolHookRegistry
from tools.daytona_toolkit.hooks.posthook import write_extend_scope
from tools.daytona_toolkit.tools import DaytonaWriteFileInput


def _ctx(write_scope: list[str] | None) -> ToolExecutionContext:
    meta: dict = {"repo_root": "/ws"}
    if write_scope is not None:
        meta["write_scope"] = list(write_scope)
    return ToolExecutionContext(cwd=Path("/ws"), metadata=meta)


def _args(path: str) -> DaytonaWriteFileInput:
    return DaytonaWriteFileInput(file_path=path, content="patched")


def _result(
    *,
    is_error: bool = False,
    changed_paths: list[str] | None = None,
) -> ToolResult:
    metadata: dict = {}
    if changed_paths is not None:
        metadata["changed_paths"] = changed_paths
    return ToolResult(output="", is_error=is_error, metadata=metadata)


def _run_hook(
    ctx: ToolExecutionContext,
    args: DaytonaWriteFileInput,
    result: ToolResult,
) -> PostHookOutcome:
    return asyncio.run(write_extend_scope.hook("daytona_write_file", args, ctx, result))


def test_extends_scope_when_write_succeeds_outside_scope() -> None:
    ctx = _ctx(["src/"])
    outcome = _run_hook(
        ctx,
        _args("/ws/other/new.py"),
        _result(changed_paths=["/ws/other/new.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/", "other/new.py"]
    assert outcome.advisories == (
        "Scope path added: other/new.py. Current scope_paths: src/, other/new.py.",
    )


def test_noop_when_result_is_error() -> None:
    ctx = _ctx(["src/"])
    _run_hook(
        ctx,
        _args("/ws/other/new.py"),
        _result(is_error=True, changed_paths=["/ws/other/new.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_changed_paths_missing() -> None:
    ctx = _ctx(["src/"])
    _run_hook(ctx, _args("/ws/other/new.py"), _result())
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_changed_paths_empty() -> None:
    ctx = _ctx(["src/"])
    _run_hook(ctx, _args("/ws/other/new.py"), _result(changed_paths=[]))
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_write_scope_absent() -> None:
    ctx = _ctx(None)
    _run_hook(
        ctx,
        _args("/ws/other/new.py"),
        _result(changed_paths=["/ws/other/new.py"]),
    )
    assert "write_scope" not in ctx.metadata


def test_noop_when_target_already_under_existing_scope() -> None:
    ctx = _ctx(["src/"])
    outcome = _run_hook(
        ctx,
        _args("/ws/src/new.py"),
        _result(changed_paths=["/ws/src/new.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]
    assert outcome.advisories == ()


def test_register_wires_hook_onto_daytona_write_file_post_bucket() -> None:
    registry = ToolHookRegistry()
    write_extend_scope.register(registry)

    entries = registry.matching("daytona_write_file", "post")
    assert len(entries) == 1
    entry = entries[0]
    assert entry.name == "daytona_write_file:extend_write_scope_on_success"
    assert entry.priority == 10
    assert entry.tool_glob == "daytona_write_file"


def test_register_does_not_match_other_tools() -> None:
    registry = ToolHookRegistry()
    write_extend_scope.register(registry)
    assert registry.matching("daytona_delete_file", "post") == []
    assert registry.matching("daytona_shell", "post") == []


def test_register_is_idempotent() -> None:
    registry = ToolHookRegistry()
    write_extend_scope.register(registry)
    write_extend_scope.register(registry)
    assert len(registry.matching("daytona_write_file", "post")) == 1
