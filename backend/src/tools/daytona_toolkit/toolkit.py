"""DaytonaToolkit — groups all Daytona sandbox tools into a single toolkit."""

from __future__ import annotations

import logging
from typing import Any

from tools.base import BaseToolkit

from tools.daytona_toolkit.tools import (
    daytona_bash,
    daytona_glob,
    daytona_grep,
    daytona_list_files,
    daytona_read_file,
    daytona_write_file,
)
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.lsp_tools import (
    daytona_lsp_definition,
    daytona_lsp_diagnostics,
    daytona_lsp_hover,
    daytona_lsp_references,
)
from tools.daytona_toolkit.codeact_tool import daytona_codeact

logger = logging.getLogger(__name__)


class DaytonaToolkit(BaseToolkit):
    """Daytona sandbox toolkit — file I/O, editing, LSP, shell, and CodeAct.

    Requires a pre-created sandbox_id. The sandbox is fetched lazily
    on first tool invocation and injected into ToolExecutionContext.metadata
    via the ``prepare_context`` helper.

    CI integration is optional — tools degrade gracefully if no
    CodeIntelligenceService is configured in the context.

    Usage::

        toolkit = DaytonaToolkit(sandbox_id="sb-abc123")
        registry.register_toolkit(toolkit)

        # Before executing tools, inject sandbox into context:
        toolkit.prepare_context(context)
    """

    def __init__(self, sandbox_id: str | None = None) -> None:
        super().__init__(
            name="sandbox_operations",
            description=(
                "Remote sandbox operations: shell, files, search, "
                "OCC-coordinated editing, LSP queries, and CodeAct execution"
            ),
            tools=[
                # Read tools first (preferred execution order)
                daytona_list_files,
                daytona_grep,
                daytona_glob,
                daytona_read_file,
                # LSP queries
                daytona_lsp_hover,
                daytona_lsp_definition,
                daytona_lsp_references,
                daytona_lsp_diagnostics,
                # Write tools
                daytona_write_file,
                daytona_edit_file,
                daytona_codeact,
                # Execution
                daytona_bash,
            ],
            instructions=(
                "Interact with a remote Daytona sandbox for file operations, "
                "code analysis, editing, and command execution. "
                "Read before you write — explore and understand context first.\n\n"
                "**Explore & Search**\n"
                "- `daytona_list_files` — list directory contents. Use to orient yourself.\n"
                "- `daytona_glob` — find files by pattern (e.g. `**/*.py`). Use to locate files.\n"
                "- `daytona_grep` — search file contents by regex. Use to find code patterns.\n"
                "- `daytona_read_file` — read a file. Use before editing to understand context.\n\n"
                "**Analyze**\n"
                "- `daytona_lsp_hover` — type info and docs for a symbol at a position.\n"
                "- `daytona_lsp_definition` — jump to where a symbol is defined.\n"
                "- `daytona_lsp_references` — find all usages of a symbol across files.\n"
                "- `daytona_lsp_diagnostics` — check a file for errors and warnings.\n\n"
                "**Edit**\n"
                "- `daytona_edit` — targeted string replacement in a file. Preferred for small changes.\n"
                "- `daytona_write_file` — create or overwrite a file. Use for new files.\n"
                "- `daytona_codeact` — execute Python with atomic file I/O. "
                "Use for multi-step transformations that need read/write/shell in one operation.\n\n"
                "**Execute**\n"
                "- `daytona_bash` — run a shell command. Use for tests, builds, installs, verification.\n\n"
                "After editing, always verify by reading the file back or running relevant tests."
            ),
        )
        self.sandbox_id = sandbox_id
        self._sandbox: Any | None = None

    def _get_sandbox(self) -> Any:
        """Lazily fetch the sandbox on first access."""
        if self._sandbox is not None:
            return self._sandbox
        if not self.sandbox_id:
            raise RuntimeError(
                "No sandbox_id configured. Pass sandbox_id to DaytonaToolkit() "
                "or set it via toolkit.sandbox_id = '...'."
            )
        from tools.daytona_toolkit.client import get_sandbox

        self._sandbox = get_sandbox(self.sandbox_id)
        logger.info("Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    async def _get_sandbox_async(self) -> Any:
        """Lazily fetch the async sandbox on first access."""
        if self._sandbox is not None:
            return self._sandbox
        if not self.sandbox_id:
            raise RuntimeError(
                "No sandbox_id configured. Pass sandbox_id to DaytonaToolkit() "
                "or set it via toolkit.sandbox_id = '...'."
            )
        from tools.daytona_toolkit.async_client import get_async_sandbox

        self._sandbox = await get_async_sandbox(self.sandbox_id)
        logger.info("Async Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    def prepare_context(self, context: Any) -> None:
        """Inject sandbox and optional CI service into a ToolExecutionContext.

        Call this before executing any Daytona tool so it can access
        the sandbox via ``context.metadata['daytona_sandbox']`` and
        optionally the CI service via ``context.metadata['ci_service']``.
        """
        sandbox = self._get_sandbox()
        context.metadata["daytona_sandbox"] = sandbox
        project_dir = getattr(sandbox, "project_dir", None)
        if project_dir:
            context.metadata["daytona_cwd"] = project_dir

        if self.sandbox_id and "ci_service" not in context.metadata:
            try:
                from code_intelligence.routing.service import get_code_intelligence

                workspace_root = project_dir or "/workspace"
                svc = get_code_intelligence(
                    sandbox_id=self.sandbox_id,
                    workspace_root=workspace_root,
                    sandbox=sandbox,
                )
                context.metadata["ci_service"] = svc
            except Exception:
                logger.debug("CI service not available for sandbox %s", self.sandbox_id)

    async def prepare_context_async(self, context: Any) -> None:
        """Inject async sandbox and optional CI service into a ToolExecutionContext.

        Use this for streaming tool execution where cancellation support is needed.
        The async sandbox supports asyncio.CancelledError propagation.
        """
        sandbox = await self._get_sandbox_async()
        context.metadata["daytona_sandbox"] = sandbox
        project_dir = getattr(sandbox, "project_dir", None)
        if project_dir:
            context.metadata["daytona_cwd"] = project_dir

        if self.sandbox_id and "ci_service" not in context.metadata:
            try:
                from code_intelligence.routing.service import get_code_intelligence

                workspace_root = project_dir or "/workspace"
                svc = get_code_intelligence(
                    sandbox_id=self.sandbox_id,
                    workspace_root=workspace_root,
                    sandbox=sandbox,
                )
                context.metadata["ci_service"] = svc
            except Exception:
                logger.debug("CI service not available for sandbox %s", self.sandbox_id)
