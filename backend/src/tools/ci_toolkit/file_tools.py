"""File-oriented CI tool — bounded file reads via CI cache."""

from __future__ import annotations

import json
import logging

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult
from ephemeralos.tools.daytona_toolkit.ci_integration import get_ci_gateway

logger = logging.getLogger(__name__)

_MAX_LINES = 500
_MAX_CHARS = 32_000


class CIReadFileInput(BaseModel):
    path: str = Field(description="File path to read")
    start_line: int = Field(default=1, ge=1, description="First line to read (1-based)")
    max_lines: int = Field(default=200, ge=1, le=_MAX_LINES, description="Maximum lines to return")


class CIReadFileTool(BaseTool):
    """Read a file from the workspace via CI cache."""

    name = "ci_read_file"
    description = "Read file contents from the workspace sandbox with line numbers."
    input_model = CIReadFileInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(
        self, arguments: CIReadFileInput, context: ToolExecutionContext,
    ) -> ToolResult:
        gw = get_ci_gateway(context)

        # Try reading from tree cache first
        content = None
        if gw:
            tc = gw.tree_cache
            if tc:
                entry = tc.get_tree(arguments.path)
                if entry:
                    content = entry.content

        # Fall back to direct file read
        if content is None:
            try:
                from pathlib import Path
                p = Path(arguments.path)
                if not p.is_file():
                    return ToolResult(output=f"File not found: {arguments.path}", is_error=True)
                content = p.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                return ToolResult(output=f"Binary file: {arguments.path}", is_error=True)
            except Exception as exc:
                return ToolResult(output=str(exc), is_error=True)

        # Truncate large files
        if len(content) > _MAX_CHARS:
            content = content[:_MAX_CHARS]
            truncated = True
        else:
            truncated = False

        lines = content.splitlines()
        total = len(lines)
        start = max(1, arguments.start_line)
        end = min(total, start + arguments.max_lines - 1)

        selected = []
        for i in range(start, end + 1):
            selected.append(f"{i:4d}: {lines[i - 1]}")

        result = {
            "file_path": arguments.path,
            "start_line": start,
            "end_line": end,
            "total_lines": total,
            "truncated": truncated,
            "content": "\n".join(selected),
        }

        return ToolResult(output=json.dumps(result, indent=2))
