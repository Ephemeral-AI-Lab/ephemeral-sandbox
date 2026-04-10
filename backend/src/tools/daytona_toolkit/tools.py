"""Daytona tool implementations — @tool-decorated functions for sandbox operations."""

from __future__ import annotations

import asyncio
import json
import logging
import re
import shlex
import uuid
from typing import Any

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from tools.daytona_toolkit.ci_integration import (
    abort_ci_write,
    command_may_mutate_workspace,
    finalize_ci_write,
    prepare_ci_write,
    prepare_declared_shell_outputs,
    release_declared_shell_outputs,
    shell_mutation_declaration_error,
    sync_shell_mutations,
    sync_write_to_ci,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

_DEFAULT_TIMEOUT = 120
_BACKGROUND_DEFAULT_TIMEOUT = 1800
_OUTPUT_MAX_CHARS = 8000
_EXIT_MARKER = "__CODEX_EXIT_CODE__="


def _truncate(text: str, max_chars: int = _OUTPUT_MAX_CHARS) -> str:
    if len(text) <= max_chars:
        return text
    half = max_chars // 2
    return text[:half] + f"\n\n... truncated ({len(text)} chars total) ...\n\n" + text[-half:]


def _wrap_bash_command(command: str) -> str:
    """Wrap *command* so we can recover exit code even if the SDK omits it."""
    script = (
        f"{command}\n"
        "__codex_exit_code=$?\n"
        f'printf "\\n{_EXIT_MARKER}%s\\n" "$__codex_exit_code"\n'
        'exit "$__codex_exit_code"'
    )
    return f"env -u LC_ALL bash -lc {shlex.quote(script)}"


def _extract_exit_code(
    output: str,
    *,
    fallback_exit_code: int | None,
) -> tuple[str, int]:
    """Strip the synthetic exit marker and return the resolved exit code."""
    match = re.search(rf"\n?{re.escape(_EXIT_MARKER)}(-?\d+)\s*$", output, flags=re.S)
    if match:
        resolved = int(match.group(1))
        cleaned = output[: match.start()]
        if cleaned.endswith("\n"):
            cleaned = cleaned[:-1]
        return cleaned, resolved
    return output, 0 if fallback_exit_code is None else int(fallback_exit_code)


def _get_sandbox(context: ToolExecutionContext) -> Any:
    """Retrieve the sandbox object from tool execution context metadata."""
    sandbox = context.metadata.get("daytona_sandbox")
    if sandbox is None:
        raise RuntimeError(
            "No Daytona sandbox in context. "
            "Ensure DaytonaToolkit was initialized with a valid sandbox_id."
        )
    return sandbox


def _path_error(exc: Exception, path: str) -> str | None:
    """Return a human-readable message if *exc* is a path-not-found error, else None."""
    msg = str(exc)
    if isinstance(exc, FileNotFoundError) or "No such file or directory" in msg:
        return f"Path does not exist: {path}"
    # Daytona SDK wraps errors and may lose the inner message
    _sdk_prefixes = ("Failed to list files", "Failed to upload files", "Failed to download")
    if any(msg.startswith(p) for p in _sdk_prefixes) and msg.rstrip().endswith(":"):
        return f"Path does not exist: {path}"
    return None


def _get_cwd(context: ToolExecutionContext) -> str | None:
    """Get working directory, preferring sandbox project dir.

    Returns None if no sandbox-specific cwd is set, letting the sandbox
    use its default directory (typically /home/daytona).
    """
    return context.metadata.get("daytona_cwd")


def _resolve_path(path: str, context: ToolExecutionContext) -> str:
    """Resolve a relative path against the sandbox cwd.

    Absolute paths are returned as-is. Relative paths are joined
    with the sandbox cwd (detected via pwd on first connect).
    """
    if path.startswith("/"):
        return path
    cwd = _get_cwd(context)
    if cwd:
        return f"{cwd}/{path}"
    return path


# ---------------------------------------------------------------------------
# Shell execution
# ---------------------------------------------------------------------------


@tool(name="daytona_bash", description="Run a shell command and return stdout and exit code.", background="optional")
async def daytona_bash(
    command: str,
    timeout: int = _DEFAULT_TIMEOUT,
    declared_output_paths: list[str] | None = None,
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
    on_progress_line = context.metadata.get("on_progress_line")
    mutates_workspace = command_may_mutate_workspace(command)
    effective_declared_output_paths = declared_output_paths if mutates_workspace else None
    declaration_error = shell_mutation_declaration_error(
        context,
        command=command,
        declared_output_paths=effective_declared_output_paths,
    )
    if declaration_error is not None:
        return ToolResult(
            output=declaration_error,
            is_error=True,
            metadata={"missing_declarations": True, "conflict": True},
        )
    if mutates_workspace or effective_declared_output_paths:
        declared_shell_prepared, scope_packet, precheck_error = prepare_declared_shell_outputs(
            context,
            declared_output_paths=effective_declared_output_paths,
        )
    else:
        declared_shell_prepared = []
        scope_packet = context.metadata.get("scope_packet")
        if not isinstance(scope_packet, dict):
            scope_packet = {}
        precheck_error = None
    if precheck_error is not None:
        return ToolResult(
            output=precheck_error,
            is_error=True,
            metadata={"scope_packet": scope_packet, "conflict": True},
        )

    wrapped = _wrap_bash_command(command)

    # Streaming path: when launched as a background task, query.py injects
    # ``on_progress_line`` into the metadata. Use a Daytona session so we can
    # tail stdout/stderr live and feed each line into the BackgroundTaskManager,
    # making the partial output visible via check_background_progress mid-run.
    if callable(on_progress_line):
        bg_timeout = timeout if timeout != _DEFAULT_TIMEOUT else _BACKGROUND_DEFAULT_TIMEOUT
        try:
            result = await _exec_streaming(
                sandbox=sandbox,
                command=wrapped,
                cwd=cwd,
                timeout=bg_timeout,
                on_progress_line=on_progress_line,
            )
            sync_info = await sync_shell_mutations(
                context,
                command=command,
                declared_output_paths=effective_declared_output_paths,
            )
            if sync_info.get("enabled"):
                try:
                    data = json.loads(result.output)
                    data["ci_sync"] = sync_info
                    return ToolResult(
                        output=json.dumps(data),
                        is_error=result.is_error,
                        metadata=dict(result.metadata),
                    )
                except Exception:
                    logger.debug("Failed to attach shell CI sync metadata", exc_info=True)
            return result
        finally:
            release_declared_shell_outputs(context, declared_shell_prepared)

    try:
        kwargs: dict[str, object] = {"timeout": timeout}
        if cwd:
            kwargs["cwd"] = cwd
        response = await sandbox.process.exec(wrapped, **kwargs)
        stdout, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        payload = {
            "cwd": cwd or "",
            "stdout": _truncate(stdout),
            "exit_code": exit_code,
        }
        try:
            sync_info = await sync_shell_mutations(
                context,
                command=command,
                declared_output_paths=effective_declared_output_paths,
            )
            if sync_info.get("enabled"):
                payload["ci_sync"] = sync_info
            output = json.dumps(payload)
            return ToolResult(
                output=output,
                is_error=exit_code != 0,
                metadata={"exit_code": exit_code},
            )
        finally:
            release_declared_shell_outputs(context, declared_shell_prepared)
    except Exception as exc:
        release_declared_shell_outputs(context, declared_shell_prepared)
        return ToolResult(output=str(exc), is_error=True)


async def _exec_streaming(
    *,
    sandbox: Any,
    command: str,
    cwd: str | None,
    timeout: int,
    on_progress_line: Any,
) -> ToolResult:
    """Run *command* via a Daytona session and stream stdout lines live.

    Each newline-terminated chunk from stdout/stderr is forwarded to
    ``on_progress_line`` so the BackgroundTaskManager can surface a live
    tail through check_background_progress while the task is still running.
    """
    from daytona_sdk import SessionExecuteRequest

    session_id = f"bash-{uuid.uuid4().hex[:12]}"
    process = sandbox.process
    poll_interval = 0.5
    deadline = asyncio.get_event_loop().time() + timeout

    last_emitted = 0  # number of stdout chars already forwarded as progress
    line_buf = ""

    def _flush_lines(new_text: str) -> None:
        nonlocal line_buf
        if not new_text:
            return
        line_buf += new_text
        while "\n" in line_buf:
            line, line_buf = line_buf.split("\n", 1)
            if line.startswith(_EXIT_MARKER):
                continue
            try:
                on_progress_line(line)
            except Exception as cb_exc:
                logger.debug("on_progress_line callback failed: %s", cb_exc)

    try:
        await process.create_session(session_id)
    except Exception as exc:
        return ToolResult(output=f"failed to create sandbox session: {exc}", is_error=True)

    final_stdout = ""
    final_stderr = ""
    exit_code: int | None = None
    try:
        full_cmd = f"cd {shlex.quote(cwd)} && {command}" if cwd else command
        req = SessionExecuteRequest(command=full_cmd, run_async=True)
        try:
            resp = await process.execute_session_command(session_id, req)
        except Exception as exc:
            return ToolResult(output=f"failed to start command: {exc}", is_error=True)

        cmd_id = getattr(resp, "cmd_id", None) or getattr(resp, "command_id", None)
        if not cmd_id:
            return ToolResult(
                output=f"daytona session did not return a cmd_id: {resp!r}",
                is_error=True,
            )

        # Poll logs and command status until the command exits.
        while True:
            try:
                logs = await process.get_session_command_logs(session_id, cmd_id)
                stdout_text = getattr(logs, "stdout", "") or ""
                stderr_text = getattr(logs, "stderr", "") or ""
            except Exception as exc:
                logger.debug("get_session_command_logs failed: %s", exc)
                stdout_text = final_stdout
                stderr_text = final_stderr

            if len(stdout_text) > last_emitted:
                new_text = stdout_text[last_emitted:]
                last_emitted = len(stdout_text)
                _flush_lines(new_text)

            final_stdout = stdout_text
            final_stderr = stderr_text

            try:
                cmd_info = await process.get_session_command(session_id, cmd_id)
                exit_code = getattr(cmd_info, "exit_code", None)
            except Exception:
                exit_code = None

            if exit_code is not None:
                break
            if asyncio.get_event_loop().time() >= deadline:
                return ToolResult(
                    output=f"command timed out after {timeout}s",
                    is_error=True,
                    metadata={"exit_code": None},
                )
            await asyncio.sleep(poll_interval)

        # One final poll to capture any tail logs written between the last
        # poll and the exit_code becoming visible.
        try:
            logs = await process.get_session_command_logs(session_id, cmd_id)
            tail_stdout = getattr(logs, "stdout", "") or ""
            tail_stderr = getattr(logs, "stderr", "") or ""
            if len(tail_stdout) > last_emitted:
                _flush_lines(tail_stdout[last_emitted:])
            final_stdout = tail_stdout
            final_stderr = tail_stderr
        except Exception as exc:
            logger.debug("final log poll failed: %s", exc)

        if line_buf:
            if not line_buf.startswith(_EXIT_MARKER):
                try:
                    on_progress_line(line_buf)
                except Exception as cb_exc:
                    logger.debug("on_progress_line callback failed (flush): %s", cb_exc)
            line_buf = ""

        cleaned_stdout, resolved_exit_code = _extract_exit_code(
            final_stdout,
            fallback_exit_code=exit_code,
        )
        output = json.dumps(
            {
                "cwd": cwd or "",
                "stdout": _truncate(cleaned_stdout),
                "exit_code": resolved_exit_code,
            }
        )
        return ToolResult(
            output=output,
            is_error=resolved_exit_code != 0,
            metadata={"exit_code": resolved_exit_code},
        )
    finally:
        try:
            await process.delete_session(session_id)
        except Exception as exc:
            logger.debug("failed to delete daytona session %s: %s", session_id, exc)


# ---------------------------------------------------------------------------
# File read
# ---------------------------------------------------------------------------


@tool(
    name="daytona_read_file",
    description="Read file contents, optionally specifying a line range.",
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
    file_path = _resolve_path(file_path, context)
    try:
        raw = await sandbox.fs.download_file(file_path)
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
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "total_lines": total,
                "start_line": start,
                "end_line": end,
                "content": _truncate("\n".join(selected)),
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        return ToolResult(output=_path_error(exc, file_path) or str(exc), is_error=True)


# ---------------------------------------------------------------------------
# File write
# ---------------------------------------------------------------------------


@tool(
    name="daytona_write_file", description="Create a new file or overwrite an existing file with the given content."
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
    file_path = _resolve_path(file_path, context)
    prepared = None
    try:
        content_bytes = content.encode("utf-8")
        # Ensure parent directories exist
        parent = "/".join(file_path.split("/")[:-1])
        if parent:
            await sandbox.process.exec(f"mkdir -p {shlex.quote(parent)}")
        prepared, scope_packet, err = prepare_ci_write(context, file_path)
        if err is not None:
            return ToolResult(
                output=err,
                is_error=True,
                metadata={"scope_packet": scope_packet, "conflict": True},
            )
        if prepared is not None:
            result = finalize_ci_write(
                context,
                prepared,
                content=content,
                edit_type="write",
                description="daytona_write_file",
            )
            if not getattr(result, "success", False):
                return ToolResult(
                    output=str(getattr(result, "message", "") or "Write failed"),
                    is_error=True,
                    metadata={"conflict": bool(getattr(result, "conflict", False))},
                )
        else:
            # SDK signature: upload_file(src: str | bytes, dst: str)
            await sandbox.fs.upload_file(content_bytes, file_path)
            sync_write_to_ci(
                context,
                file_path,
                content,
                edit_type="write",
                description="daytona_write_file",
            )
        output = json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "bytes_written": len(content_bytes),
                "ci_sync": True,
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        parent = "/".join(file_path.split("/")[:-1])
        return ToolResult(output=_path_error(exc, parent) or str(exc), is_error=True)
    finally:
        abort_ci_write(context, prepared)


# ---------------------------------------------------------------------------
# List files
# ---------------------------------------------------------------------------


@tool(
    name="daytona_list_files",
    description="List files and directories in a given path.",
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
    directory = _resolve_path(directory, context) if directory != "." else (_get_cwd(context) or ".")
    try:
        entries = await sandbox.fs.list_files(directory)
        names = []
        for entry in entries or []:
            name = getattr(entry, "name", None) or str(entry)
            names.append(name)
        output = json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "directory": directory,
                "entries": sorted(names),
            }
        )
        return ToolResult(output=output)
    except Exception as exc:
        return ToolResult(output=_path_error(exc, directory) or str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Grep search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_grep",
    description="Search file contents for a text pattern and return matching lines.",
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
    cwd = _get_cwd(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        matches = await sandbox.fs.find_files(path, pattern)
        if not matches:
            return ToolResult(
                output=json.dumps(
                    {
                        "cwd": cwd,
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
                    "cwd": cwd,
                    "pattern": pattern,
                    "path": path,
                    "matches": result_matches,
                    "total_matches": len(matches),
                }
            )
        )
    except Exception as exc:
        return ToolResult(output=_path_error(exc, path) or str(exc), is_error=True)


# ---------------------------------------------------------------------------
# Glob search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_glob",
    description="Find files by name using a glob pattern (e.g. '*.py', 'test_*').",
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
    cwd = _get_cwd(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        # Use shell find for reliable glob — SDK search_files has issues
        # Strip leading **/ from glob patterns for find -name compatibility
        find_pattern = pattern.replace("**/", "")
        cmd = f"find {path} -name {find_pattern} -type f"
        resp = await sandbox.process.exec(cmd, timeout=30)
        file_list = [f for f in (resp.result or "").splitlines() if f.strip()][:500]
        return ToolResult(
            output=json.dumps(
                {
                    "cwd": cwd,
                    "pattern": pattern,
                    "path": path,
                    "files": file_list,
                    "total_files": len(file_list),
                }
            )
        )
    except Exception as exc:
        return ToolResult(output=_path_error(exc, path) or str(exc), is_error=True)
