"""Daytona sandbox tools for search, file changes, and commands."""

from __future__ import annotations

import logging
from typing import Any

from tools.core.base import BaseToolkit

from tools.daytona_toolkit.tools import (
    daytona_glob,
    daytona_grep,
    daytona_read_file,
    daytona_write_file,
)
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.codeact_tool import daytona_codeact
from tools.daytona_toolkit.rename_tool import daytona_rename_symbol
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)

# Guard registration happens in tools.daytona_toolkit.__init__.py (imported
# transitively by any daytona_toolkit module).

logger = logging.getLogger(__name__)


def _build_tools(*, include_codeact: bool) -> list[Any]:
    tools: list[Any] = [
        daytona_grep,
        daytona_glob,
        daytona_read_file,
        daytona_write_file,
        daytona_edit_file,
        daytona_rename_symbol,
        daytona_delete_file,
        daytona_move_file,
    ]
    if include_codeact:
        tools.append(daytona_codeact)
    return tools


def _build_instructions(*, include_codeact: bool) -> str:
    codeact_section = ""
    if include_codeact:
        codeact_section = (
            "\n**Run Commands**\n"
            "- `daytona_codeact`: run tests, builds, and other runtime commands.\n"
            "- Use `command=\"pytest ...\"` for shell commands.\n"
            "- Use `code=\"...\"` only for multi-step Python.\n"
            "- Commands already start at the repo root; do not prefix `cd /testbed &&` or `cd /workspace &&`.\n"
            "- Test and benchmark commands must not contain `|`, `>`, `2>&1`, `2>/dev/null`, `head`, or `tail`; output is captured automatically.\n"
            "- Do not use CodeAct for file writes, moves, deletes, or file-content reads.\n"
            "- Use the edit, write, rename, delete, move, read, grep, or glob tools for file work.\n"
            "- Background Python should use `python -u` or `print(..., flush=True)`.\n"
        )
    return (
        "Use these tools inside the remote Daytona sandbox.\n"
        "Use repo-relative paths or `/testbed/...` paths. Never use host paths such as `/Users/...`.\n"
        "In team lanes, call the required Task Center tools before any Daytona tool.\n"
        "Do not call a Daytona tool in the same assistant action as `load_skill`.\n"
        "Use CI/navigation tools first when available; use file reads after you know the target path or line range.\n\n"
        "**Find And Read**\n"
        "- `daytona_glob`: find files by glob, such as `**/*.py`.\n"
        "- `daytona_grep`: search file contents by regex.\n"
        "- `daytona_read_file`: read one file or a bounded line range.\n\n"
        "**Change Files**\n"
        "- `daytona_edit_file`: replace exact text. Use exactly one mode: `old_text` + `new_text`, or an `edits` list.\n"
        "- `daytona_write_file`: create or overwrite a file. There is no `write_file` tool; do not call `write_file`.\n"
        "- `daytona_rename_symbol`: rename a Python symbol across references. Add `kind` or `file_hint` if the name is ambiguous.\n"
        "- `daytona_delete_file`: delete a file. Set `is_folder=true` to delete a folder tree.\n"
        "- `daytona_move_file`: move a file. Set `is_folder=true` to move a folder tree. Use this instead of `mv`.\n"
        "- Team lanes block test-file writes unless runtime metadata allows them.\n"
        "- Outside-scope production edits need a clear ownership reason and verification.\n"
        "- If scope is unclear, request replanning instead of guessing.\n"
        f"{codeact_section}"
    )


class DaytonaToolkit(BaseToolkit):
    """Toolkit for running Daytona sandbox tools.

    Pass a sandbox id. The toolkit fetches the sandbox lazily and stores it in
    execution metadata before tools run.
    """

    @classmethod
    def from_context(cls, ctx: Any) -> DaytonaToolkit:
        sandbox_id = ctx.metadata.get("sandbox_id", "") if ctx is not None else ""
        return cls(sandbox_id=sandbox_id or None)

    def __init__(
        self,
        sandbox_id: str | None = None,
        *,
        include_codeact: bool = True,
    ) -> None:
        super().__init__(
            name="sandbox_operations",
            description="Remote Daytona sandbox tools for search, file changes, and commands.",
            tools=_build_tools(include_codeact=include_codeact),
            instructions=_build_instructions(include_codeact=include_codeact),
        )
        self.sandbox_id = sandbox_id
        self._sandbox: Any | None = None
        self._sandbox_loop_id: int | None = None

    def _get_sandbox(self) -> Any:
        """Fetch the sandbox once and cache it."""
        if self._sandbox is not None:
            return self._sandbox
        if not self.sandbox_id:
            raise RuntimeError(
                "No sandbox_id configured. Pass sandbox_id to DaytonaToolkit() "
                "or set it via toolkit.sandbox_id = '...'."
            )
        from sandbox import fetch_sandbox as get_sandbox

        self._sandbox = get_sandbox(self.sandbox_id)
        logger.debug("Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    async def _get_sandbox_async(self) -> Any:
        """Fetch the async sandbox once per event loop."""
        import asyncio

        loop_id = id(asyncio.get_running_loop())
        if self._sandbox is not None and self._sandbox_loop_id == loop_id:
            return self._sandbox
        # Stale sandbox from a different (possibly closed) loop — discard it
        self._sandbox = None
        self._sandbox_loop_id = None
        if not self.sandbox_id:
            raise RuntimeError(
                "No sandbox_id configured. Pass sandbox_id to DaytonaToolkit() "
                "or set it via toolkit.sandbox_id = '...'."
            )
        from sandbox.async_client import get_async_sandbox

        self._sandbox = await get_async_sandbox(self.sandbox_id)
        self._sandbox_loop_id = loop_id
        logger.debug("Async Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    @staticmethod
    def _resolve_cwd_sync(sandbox: Any) -> str | None:
        from sandbox.workspace import discover_workspace

        return discover_workspace(sandbox)

    @staticmethod
    async def _resolve_cwd_async(sandbox: Any) -> str | None:
        from sandbox.workspace import discover_workspace_async

        return await discover_workspace_async(sandbox)

    def prepare_context(self, context: Any) -> None:
        """Add the sandbox and repo root to tool execution metadata."""
        sandbox = self._get_sandbox()
        repo_root = context.metadata.get("repo_root") or self._resolve_cwd_sync(sandbox)
        from sandbox.workspace import ensure_code_intelligence_runtime

        ensure_code_intelligence_runtime(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )

    async def prepare_context_async(self, context: Any) -> None:
        """Add the async sandbox and repo root to tool execution metadata."""
        sandbox = await self._get_sandbox_async()
        repo_root = context.metadata.get("repo_root") or await self._resolve_cwd_async(sandbox)
        from sandbox.workspace import ensure_code_intelligence_runtime

        ensure_code_intelligence_runtime(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )
