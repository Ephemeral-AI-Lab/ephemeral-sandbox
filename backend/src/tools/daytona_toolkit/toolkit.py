"""Daytona sandbox tools — file I/O, editing, and CodeAct execution."""

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
    codeact_line = ""
    if include_codeact:
        codeact_line = (
            "- `daytona_codeact` — execute direct shell commands or Python in the repo workspace. "
            "Use `daytona_codeact(command=\"pytest ...\", timeout=N)` for tests, builds, and "
            "verification, and `daytona_codeact(code=\"...\")` for multi-step Python that needs "
            "read/shell in one operation. Do not use CodeAct for file mutations: no `sed -i`, "
            "output redirects, `tee`, inline Python writes, `rm`, `mv`, `unlink`, `os.remove`, "
            "`Path.unlink`, `shutil.rmtree`, `shutil.move`, `os.rename`, `git rm`, or `git mv`. Use `daytona_edit_file`, "
            "`daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, or "
            "`daytona_move_file` instead; those tools own the audited write path. "
            "Keep commands repo-root-relative; do not import `subprocess` and do not append "
            "stdout/stderr capture plumbing such as `2>&1` or `2>/dev/null`.\n"
        )
    return (
        "Interact with a remote Daytona sandbox for file operations, "
        "code analysis, editing, and command execution. "
        "Use CI/navigation tools first when they are available; use sandbox "
        "file reads only after CI or search narrowed the seam.\n\n"
        "**Explore & Search**\n"
        "- `daytona_glob` — find files by pattern (e.g. `**/*.py`). Use to locate files.\n"
        "- `daytona_grep` — search file contents by regex. Use to find code patterns.\n"
        "- `daytona_read_file` — read a file. Use after CI/search narrowed the target and before editing exact lines.\n\n"
        "**Edit**\n"
        "- `daytona_edit_file` — atomic file edits. Use exactly one mode: "
        "`old_text` + `new_text` for a single replacement, or "
        "`edits=[{\"strategy\":\"search_replace\",\"search\":\"...\",\"replace\":\"...\"}]` for batched replacements. "
        "Never send `new_text` together with `edits`. In coordinated team lanes, test files are "
        "read/verify-only and test-file writes are blocked unless explicit authorization is present.\n"
        "- `daytona_write_file` — create or overwrite a file. Use for new files. "
        "The tool is named exactly `daytona_write_file`; do not call `write_file`, `Write`, or any unprefixed file tool. "
        "In coordinated team lanes, do not create or overwrite test files unless explicit authorization is present.\n"
        "- `daytona_rename_symbol` — rename a Python function, class, method, or import binding "
        "across definitions, call sites, and imports as one audited process operation. "
        "Use this instead of chained `daytona_edit_file` calls for multi-file renames; "
        "try `dry_run=true` first when the blast radius is unclear.\n"
        "- `daytona_delete_file` — delete one file through the OCC-gated code-intelligence commit path. "
        "Base-hash drift returns `aborted_version` with no merge fallback. Recursive directory deletes are rejected "
        "until directory-tree OCC support exists. Use this instead of `rm` in CodeAct; the shell policy blocks `rm` "
        "for that reason.\n"
        "- `daytona_move_file` — move one file through the OCC-gated code-intelligence commit path. "
        "Base-hash drift on source or destination returns `aborted_version` with no merge fallback. Recursive "
        "directory moves are rejected until directory-tree OCC support exists. Use this instead of `mv` in CodeAct. "
        "By default `dst` must not exist; pass `overwrite=true` to replace it under strict destination-base checks.\n"
        f"{codeact_line}\n"
        "**Execute**\n"
        "- Use `daytona_codeact` for all runtime execution (tests, builds, verification).\n"
        "- Do not use `daytona_codeact` to edit, remove, move, or explicitly clean up files; this includes `rm`, `mv`, `unlink`, `os.remove`, `Path.unlink`, `shutil.rmtree`, `shutil.move`, `os.rename`, `git rm`, and `git mv`. Coordinated lanes block shell and Python edit side channels before execution.\n"
        "- When an injected sandbox cwd/repo root is configured, shell and file tools already run relative to that root. Prefer relative repo paths."
    )


class DaytonaToolkit(BaseToolkit):
    """Daytona sandbox toolkit — file I/O, editing, and CodeAct.

    Requires a pre-created sandbox_id. The sandbox is fetched lazily
    on first tool invocation and injected into ToolExecutionContext.metadata
    via the ``prepare_context`` helper.

    Usage::

        toolkit = DaytonaToolkit(sandbox_id="sb-abc123")
        registry.register_toolkit(toolkit)

        # Before executing tools, inject sandbox into context:
        toolkit.prepare_context(context)
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
            description="Remote sandbox operations: files, search, editing, and CodeAct execution",
            tools=_build_tools(include_codeact=include_codeact),
            instructions=_build_instructions(include_codeact=include_codeact),
        )
        self.sandbox_id = sandbox_id
        self._sandbox: Any | None = None
        self._sandbox_loop_id: int | None = None

    def _get_sandbox(self) -> Any:
        """Lazily fetch the sandbox on first access."""
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
        """Lazily fetch the async sandbox on first access.

        Invalidates the cached sandbox when the event loop changes
        (e.g. pytest-asyncio creates a new loop per test).
        """
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
        """Inject sandbox, repo root, and optional CI service into a ToolExecutionContext.

        Call this before executing any Daytona tool so it can access
        the sandbox via ``context.metadata['daytona_sandbox']`` and
        the resolved repo root via ``context.metadata['repo_root']``.
        """
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
        """Inject async sandbox, repo root, and optional CI service into a ToolExecutionContext.

        Use this for streaming tool execution where cancellation support is needed.
        The async sandbox supports asyncio.CancelledError propagation.
        """
        sandbox = await self._get_sandbox_async()
        repo_root = context.metadata.get("repo_root") or await self._resolve_cwd_async(sandbox)
        from sandbox.workspace import ensure_code_intelligence_runtime

        ensure_code_intelligence_runtime(
            context,
            sandbox_id=self.sandbox_id,
            sandbox=sandbox,
            workspace_root=repo_root,
        )
