"""Daytona tool implementations — @tool-decorated functions for sandbox operations."""

from __future__ import annotations

import json
import logging
from typing import Any

from tools.base import ToolExecutionContext, ToolResult
from tools.decorator import tool

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


@tool(name="daytona_bash", description="Run a shell command inside the remote Daytona sandbox.")
async def daytona_bash(
    command: str,
    timeout: int = _DEFAULT_TIMEOUT,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Execute a shell command in a Daytona sandbox.

    Args:
        command: Shell command to execute in the sandbox
        timeout: Timeout in seconds

    Returns:
        stdout (str): Standard output from the command
        exit_code (int): Exit code (0 = success)
    """
    sandbox = _get_sandbox(context)
    cwd = _get_cwd(context)
    try:
        response = await sandbox.process.exec(
            command,
            cwd=cwd,
            timeout=timeout,
        )
        exit_code = getattr(response, "exit_code", 0)
        output = json.dumps(
            {
                "stdout": _truncate(response.result or ""),
                "exit_code": exit_code,
            }
        )
        return ToolResult(
            output=output,
            is_error=exit_code != 0,
            metadata={"exit_code": exit_code},
        )
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# File read
# ---------------------------------------------------------------------------


@tool(
    name="daytona_read_file",
    description="Read file contents from the remote Daytona sandbox.",
    read_only=True,
)
async def daytona_read_file(
    file_path: str,
    start_line: int = 1,
    end_line: int | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Read a file from the Daytona sandbox.

    Args:
        file_path: Path to the file in the sandbox
        start_line: First line to read (1-based)
        end_line: Last line to read (1-based, inclusive)

    Returns:
        file_path (str): Path to the file
        total_lines (int): Total number of lines in the file
        start_line (int): First line returned (1-based)
        end_line (int): Last line returned (1-based)
        content (str): File content with line numbers
    """
    sandbox = _get_sandbox(context)
    try:
        raw = sandbox.fs.download_file(file_path)
        content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
        lines = content.splitlines()
        total = len(lines)

        start = max(1, start_line)
        end = min(total, end_line) if end_line else total

        selected = []
        for i in range(start, end + 1):
            selected.append(f"{i:4d}: {lines[i - 1]}")

        output = json.dumps(
            {
                "file_path": file_path,
                "total_lines": total,
                "start_line": start,
                "end_line": end,
                "content": _truncate("\n".join(selected)),
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# File write
# ---------------------------------------------------------------------------


@tool(
    name="daytona_write_file", description="Write or create a file in the remote Daytona sandbox."
)
async def daytona_write_file(
    file_path: str,
    content: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Write/create a file in the Daytona sandbox.

    Args:
        file_path: Path to write in the sandbox
        content: File content to write

    Returns:
        file_path (str): Path that was written
        bytes_written (int): Number of bytes written
    """
    sandbox = _get_sandbox(context)
    try:
        content_bytes = content.encode("utf-8")
        await sandbox.fs.upload_file(file_path, content_bytes)
        output = json.dumps(
            {
                "file_path": file_path,
                "bytes_written": len(content_bytes),
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# List files
# ---------------------------------------------------------------------------


@tool(
    name="daytona_list_files",
    description="List files and directories in the remote Daytona sandbox.",
    read_only=True,
)
async def daytona_list_files(
    directory: str = ".",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """List files in a directory in the Daytona sandbox.

    Args:
        directory: Directory path to list

    Returns:
        directory (str): Directory that was listed
        entries (list): File and directory names
    """
    sandbox = _get_sandbox(context)
    cwd = _get_cwd(context)
    directory = directory if directory != "." else cwd
    try:
        entries = sandbox.fs.list_files(directory)
        names = []
        for entry in entries or []:
            name = getattr(entry, "name", None) or str(entry)
            names.append(name)
        output = json.dumps(
            {
                "directory": directory,
                "entries": sorted(names),
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Grep search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_grep",
    description="Search file contents for a pattern in the remote Daytona sandbox.",
    read_only=True,
)
async def daytona_grep(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Search file contents in the Daytona sandbox.

    Args:
        pattern: Text pattern to search for in file contents
        path: File or directory to search

    Returns:
        pattern (str): Pattern that was searched
        path (str): Search root path
        matches (list): Matching results with file, line, content
        total_matches (int): Total matches found
    """
    sandbox = _get_sandbox(context)
    cwd = _get_cwd(context)
    path = path if path != "." else cwd
    try:
        matches = sandbox.fs.find_files(path, pattern)
        if not matches:
            return ToolResult(
                output=json.dumps(
                    {
                        "pattern": pattern,
                        "path": path,
                        "matches": [],
                        "total_matches": 0,
                    }
                )
            )
        result_matches = []
        for match in matches[:500]:
            file_path = getattr(match, "file", None) or ""
            line_no = getattr(match, "line", None)
            content = getattr(match, "content", None) or ""
            result_matches.append(
                {
                    "file": file_path,
                    "line": line_no,
                    "content": content.rstrip(),
                }
            )
        return ToolResult(
            output=json.dumps(
                {
                    "pattern": pattern,
                    "path": path,
                    "matches": result_matches,
                    "total_matches": len(matches),
                }
            )
        )
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Glob search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_glob",
    description="Find files matching a glob pattern in the remote Daytona sandbox.",
    read_only=True,
)
async def daytona_glob(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find files by glob pattern in the Daytona sandbox.

    Args:
        pattern: Glob pattern to match file names (e.g. '*.py', 'test_*')
        path: Root directory to search from

    Returns:
        pattern (str): Glob pattern used
        path (str): Search root path
        files (list): Matching file paths
        total_files (int): Total files found
    """
    sandbox = _get_sandbox(context)
    cwd = _get_cwd(context)
    path = path if path != "." else cwd
    try:
        response = sandbox.fs.search_files(path, pattern)
        files = getattr(response, "files", None) or []
        return ToolResult(
            output=json.dumps(
                {
                    "pattern": pattern,
                    "path": path,
                    "files": files[:500],
                    "total_files": len(files),
                }
            )
        )
    except Exception as exc:
        # Fallback: use shell glob via process.exec
        try:
            fallback_cmd = f"find {path} -name '{pattern}' 2>/dev/null | head -500"
            resp = sandbox.process.exec(fallback_cmd, cwd=cwd, timeout=30)
            file_list = [f for f in (resp.result or "").splitlines() if f.strip()]
            return ToolResult(
                output=json.dumps(
                    {
                        "pattern": pattern,
                        "path": path,
                        "files": file_list,
                        "total_files": len(file_list),
                    }
                )
            )
        except Exception as fallback_exc:
            return ToolResult(output=str(fallback_exc), is_error=True)
