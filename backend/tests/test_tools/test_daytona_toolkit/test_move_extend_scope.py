"""Tests for the move-scope-extension post-hook.

Covers the unit contract of
``tools.daytona_toolkit.hooks.posthook.move_extend_scope``: widen
``write_scope`` to the resolved ``target_path`` only when (a) the move
succeeded, (b) the committed set is non-empty, and (c) ``src_path`` was
already inside the caller's scope.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import PostHookOutcome, ToolHookRegistry
from tools.daytona_toolkit.delete_move_tool import DaytonaMoveFileInput, daytona_move_file
from tools.daytona_toolkit.hooks.posthook import move_extend_scope


def _ctx(write_scope: list[str] | None) -> ToolExecutionContext:
    meta: dict = {"repo_root": "/ws"}
    if write_scope is not None:
        meta["write_scope"] = list(write_scope)
    return ToolExecutionContext(cwd=Path("/ws"), metadata=meta)


def _args(src: str, dst: str) -> DaytonaMoveFileInput:
    return DaytonaMoveFileInput(src_path=src, target_path=dst)


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
    args: DaytonaMoveFileInput,
    result: ToolResult,
) -> PostHookOutcome:
    return asyncio.run(move_extend_scope.hook("daytona_move_file", args, ctx, result))


# ---------------------------------------------------------------------------
# Happy path: src in scope, commit succeeded → scope widens to dst
# ---------------------------------------------------------------------------


def test_extends_scope_when_src_in_scope_and_move_succeeded() -> None:
    ctx = _ctx(["src/"])
    outcome = _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/other/b.py"),
        _result(changed_paths=["/ws/other/b.py"]),
    )
    assert "other/b.py" in ctx.metadata["write_scope"]
    assert outcome.advisories == (
        "Scope path added: other/b.py. Current scope_paths: src/, other/b.py.",
    )


def test_extends_scope_for_folder_move_to_dst_root() -> None:
    """Folder move: dst root is appended; members fall under the prefix."""
    ctx = _ctx(["pkg/"])
    outcome = _run_hook(
        ctx,
        _args("/ws/pkg", "/ws/moved_pkg"),
        _result(changed_paths=["/ws/moved_pkg/a.py", "/ws/moved_pkg/sub/b.py"]),
    )
    assert "moved_pkg" in ctx.metadata["write_scope"]
    assert outcome.advisories == (
        "Scope path added: moved_pkg. Current scope_paths: pkg/, moved_pkg.",
    )


# ---------------------------------------------------------------------------
# Gates: nothing happens when the commit didn't land in the caller's scope
# ---------------------------------------------------------------------------


def test_noop_when_result_is_error() -> None:
    ctx = _ctx(["src/"])
    _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/other/b.py"),
        _result(is_error=True, changed_paths=["/ws/other/b.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_changed_paths_missing() -> None:
    ctx = _ctx(["src/"])
    _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/other/b.py"),
        _result(),
    )
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_changed_paths_empty() -> None:
    ctx = _ctx(["src/"])
    _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/other/b.py"),
        _result(changed_paths=[]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_src_outside_scope() -> None:
    """Scope doesn't follow foreign moves — only extends for owned sources."""
    ctx = _ctx(["src/"])
    _run_hook(
        ctx,
        _args("/ws/other/a.py", "/ws/elsewhere/b.py"),
        _result(changed_paths=["/ws/elsewhere/b.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]


def test_noop_when_write_scope_absent() -> None:
    """No scope configured → nothing to extend."""
    ctx = _ctx(None)
    _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/other/b.py"),
        _result(changed_paths=["/ws/other/b.py"]),
    )
    assert "write_scope" not in ctx.metadata


def test_noop_when_dst_already_under_existing_scope() -> None:
    """Rename within an already-owned prefix is a no-op for scope."""
    ctx = _ctx(["src/"])
    outcome = _run_hook(
        ctx,
        _args("/ws/src/a.py", "/ws/src/b.py"),
        _result(changed_paths=["/ws/src/b.py"]),
    )
    assert ctx.metadata["write_scope"] == ["src/"]
    assert outcome.advisories == ()


# ---------------------------------------------------------------------------
# Registration contract
# ---------------------------------------------------------------------------


def test_register_wires_hook_onto_daytona_move_file_post_bucket() -> None:
    registry = ToolHookRegistry()
    move_extend_scope.register(registry)

    entries = registry.matching("daytona_move_file", "post")
    assert len(entries) == 1
    entry = entries[0]
    assert entry.name == "daytona_move_file:extend_write_scope_on_success"
    assert entry.priority == 10
    assert entry.tool_glob == "daytona_move_file"


def test_register_does_not_match_other_tools() -> None:
    registry = ToolHookRegistry()
    move_extend_scope.register(registry)
    assert registry.matching("daytona_delete_file", "post") == []
    assert registry.matching("daytona_shell", "post") == []


def test_register_is_idempotent() -> None:
    registry = ToolHookRegistry()
    move_extend_scope.register(registry)
    move_extend_scope.register(registry)
    assert len(registry.matching("daytona_move_file", "post")) == 1


def test_move_schema_is_short_and_actionable() -> None:
    description = daytona_move_file.to_api_schema()["description"]
    assert "Move a sandbox file" in description
    assert "Set `is_folder=True` to move a folder tree" in description
    assert "Use this instead of `mv`" in description
