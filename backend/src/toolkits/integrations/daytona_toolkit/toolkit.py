"""DaytonaToolkit — groups all Daytona sandbox tools into a single toolkit."""

from __future__ import annotations

import logging
from typing import Any

from ephemeralos.tools.base import BaseToolkit

from ephemeralos.toolkits.integrations.daytona_toolkit.tools import (
    DaytonaBashTool,
    DaytonaFileReadTool,
    DaytonaFileWriteTool,
    DaytonaGlobTool,
    DaytonaGrepTool,
    DaytonaListFilesTool,
)

logger = logging.getLogger(__name__)


class DaytonaToolkit(BaseToolkit):
    """Daytona sandbox toolkit — file I/O, shell execution, and search.

    Requires a pre-created sandbox_id. The sandbox is fetched lazily
    on first tool invocation and injected into ToolExecutionContext.metadata
    via the ``prepare_context`` helper.

    Usage::

        toolkit = DaytonaToolkit(sandbox_id="sb-abc123")
        registry.register_toolkit(toolkit)

        # Before executing tools, inject sandbox into context:
        toolkit.prepare_context(context)
    """

    def __init__(self, sandbox_id: str | None = None) -> None:
        super().__init__(
            name="daytona",
            description="Remote sandbox operations via Daytona: shell, files, search",
            tools=[
                DaytonaBashTool(),
                DaytonaFileReadTool(),
                DaytonaFileWriteTool(),
                DaytonaListFilesTool(),
                DaytonaGrepTool(),
                DaytonaGlobTool(),
            ],
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
        from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_sandbox

        self._sandbox = get_sandbox(self.sandbox_id)
        logger.info("Daytona sandbox fetched: %s", self.sandbox_id)
        return self._sandbox

    def prepare_context(self, context: Any) -> None:
        """Inject sandbox into a ToolExecutionContext's metadata.

        Call this before executing any Daytona tool so it can access
        the sandbox via ``context.metadata['daytona_sandbox']``.
        """
        sandbox = self._get_sandbox()
        context.metadata["daytona_sandbox"] = sandbox
        # Set working directory to project dir if available
        project_dir = getattr(sandbox, "project_dir", None)
        if project_dir:
            context.metadata["daytona_cwd"] = project_dir
