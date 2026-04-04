"""Daytona tool implementations — BaseTool subclasses for sandbox operations."""

from __future__ import annotations

import json
import logging
from typing import Any

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

_DEFAULT_TIMEOUT = 120
_OUTPUT_MAX_CHARS = 8000


def _truncate(text: str, max_chars: int = _OUTPUT_MAX_CHARS) -> str:
    if len(text) <= max_chars:
        return text
    half = max_chars // 2
    return text[:half] + f"\n\n... truncated ({len(text)} chars total) ...\n\n" + text[-half:]


def _get_sandbox(context: ToolExecutionContext) -> Any:
    """Retrieve the sandbox object from tool execution context metadata."""
    sandbox = context.metadata.get("daytona_sandbox")
    if sandbox is None:
        raise RuntimeError(
            "No Daytona sandbox in context. "
            "Ensure DaytonaToolkit was initialized with a valid sandbox_id."
        )
    return sandbox


def _get_cwd(context: ToolExecutionContext) -> str:
    """Get working directory, preferring sandbox project dir."""
    return context.metadata.get("daytona_cwd", str(context.cwd))


# ---------------------------------------------------------------------------
# Shell execution
# ---------------------------------------------------------------------------


class DaytonaBashInput(BaseModel):
    command: str = Field(description="Shell command to execute in the sandbox")
    timeout: int = Field(
        default=_DEFAULT_TIMEOUT,
        ge=1,
        le=600,
        description="Timeout in seconds",
    )


class DaytonaBashTool(BaseTool):
    """Execute a shell command in a Daytona sandbox."""

    name = "daytona_bash"
    description = "Run a shell command inside the remote Daytona sandbox."
    input_model = DaytonaBashInput

    async def execute(self, arguments: DaytonaBashInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        cwd = _get_cwd(context)
        try:
            response = sandbox.process.exec(
                arguments.command,
                cwd=cwd,
                timeout=arguments.timeout,
            )
            output = _truncate(response.result or "")
            is_error = getattr(response, "exit_code", 0) != 0
            return ToolResult(
                output=output,
                is_error=is_error,
                metadata={"exit_code": getattr(response, "exit_code", None)},
            )
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# File read
# ---------------------------------------------------------------------------


class DaytonaFileReadInput(BaseModel):
    file_path: str = Field(description="Path to the file in the sandbox")
    start_line: int = Field(default=1, ge=1, description="First line to read (1-based)")
    end_line: int | None = Field(default=None, description="Last line to read (1-based, inclusive)")


class DaytonaFileReadTool(BaseTool):
    """Read a file from the Daytona sandbox."""

    name = "daytona_read_file"
    description = "Read file contents from the remote Daytona sandbox."
    input_model = DaytonaFileReadInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: DaytonaFileReadInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        try:
            raw = sandbox.fs.download_file(arguments.file_path)
            content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
            lines = content.splitlines()
            total = len(lines)

            start = max(1, arguments.start_line)
            end = min(total, arguments.end_line) if arguments.end_line else total

            selected = []
            for i in range(start, end + 1):
                selected.append(f"{i:4d}: {lines[i - 1]}")

            header = f"File: {arguments.file_path} ({total} lines total)\n"
            header += f"Showing lines {start}-{end}\n---\n"
            output = header + "\n".join(selected)
            return ToolResult(output=_truncate(output))
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# File write
# ---------------------------------------------------------------------------


class DaytonaFileWriteInput(BaseModel):
    file_path: str = Field(description="Path to write in the sandbox")
    content: str = Field(description="File content to write")


class DaytonaFileWriteTool(BaseTool):
    """Write/create a file in the Daytona sandbox."""

    name = "daytona_write_file"
    description = "Write or create a file in the remote Daytona sandbox."
    input_model = DaytonaFileWriteInput

    async def execute(self, arguments: DaytonaFileWriteInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        try:
            content_bytes = arguments.content.encode("utf-8")
            sandbox.fs.upload_file(arguments.file_path, content_bytes)
            return ToolResult(output=f"Written: {arguments.file_path} ({len(content_bytes)} bytes)")
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# List files
# ---------------------------------------------------------------------------


class DaytonaListFilesInput(BaseModel):
    directory: str = Field(default=".", description="Directory path to list")


class DaytonaListFilesTool(BaseTool):
    """List files in a directory in the Daytona sandbox."""

    name = "daytona_list_files"
    description = "List files and directories in the remote Daytona sandbox."
    input_model = DaytonaListFilesInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: DaytonaListFilesInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        cwd = _get_cwd(context)
        directory = arguments.directory if arguments.directory != "." else cwd
        try:
            entries = sandbox.fs.list_files(directory)
            if not entries:
                return ToolResult(output=f"Empty directory: {directory}")
            # entries may be strings or objects with a name attribute
            names = []
            for entry in entries:
                name = getattr(entry, "name", None) or str(entry)
                names.append(name)
            output = "\n".join(sorted(names))
            return ToolResult(output=_truncate(output))
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Grep search
# ---------------------------------------------------------------------------


class DaytonaGrepInput(BaseModel):
    pattern: str = Field(description="Text pattern to search for in file contents")
    path: str = Field(default=".", description="File or directory to search")


class DaytonaGrepTool(BaseTool):
    """Search file contents in the Daytona sandbox."""

    name = "daytona_grep"
    description = "Search file contents for a pattern in the remote Daytona sandbox."
    input_model = DaytonaGrepInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: DaytonaGrepInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        cwd = _get_cwd(context)
        path = arguments.path if arguments.path != "." else cwd
        try:
            matches = sandbox.fs.find_files(path, arguments.pattern)
            if not matches:
                return ToolResult(output=f"No matches for '{arguments.pattern}' in {path}")
            lines = []
            for match in matches[:500]:
                file_path = getattr(match, "file", None) or ""
                line_no = getattr(match, "line", None)
                content = getattr(match, "content", None) or ""
                if line_no is not None:
                    lines.append(f"{file_path}:{line_no}: {content.rstrip()}")
                else:
                    lines.append(f"{file_path}: {content.rstrip()}")
            if len(matches) > 500:
                lines.append(f"\n... truncated ({len(matches)} total, showing first 500)")
            return ToolResult(output=_truncate("\n".join(lines)))
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Glob search
# ---------------------------------------------------------------------------


class DaytonaGlobInput(BaseModel):
    pattern: str = Field(description="Glob pattern to match file names (e.g. '*.py', 'test_*')")
    path: str = Field(default=".", description="Root directory to search from")


class DaytonaGlobTool(BaseTool):
    """Find files by glob pattern in the Daytona sandbox."""

    name = "daytona_glob"
    description = "Find files matching a glob pattern in the remote Daytona sandbox."
    input_model = DaytonaGlobInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: DaytonaGlobInput, context: ToolExecutionContext) -> ToolResult:
        sandbox = _get_sandbox(context)
        cwd = _get_cwd(context)
        path = arguments.path if arguments.path != "." else cwd
        try:
            response = sandbox.fs.search_files(path, arguments.pattern)
            files = getattr(response, "files", None) or []
            if not files:
                return ToolResult(output=f"No files matching '{arguments.pattern}' in {path}")
            output = "\n".join(files[:500])
            if len(files) > 500:
                output += f"\n\n... truncated ({len(files)} total, showing first 500)"
            return ToolResult(output=_truncate(output))
        except Exception as exc:
            # Fallback: use shell glob via process.exec
            try:
                fallback_cmd = f"find {path} -name '{arguments.pattern}' 2>/dev/null | head -500"
                resp = sandbox.process.exec(fallback_cmd, cwd=cwd, timeout=30)
                return ToolResult(output=resp.result or f"No files matching '{arguments.pattern}'")
            except Exception as fallback_exc:
                return ToolResult(output=str(fallback_exc), is_error=True)
