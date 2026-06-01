"""Write bytes to an active PTY command."""

from __future__ import annotations

from pydantic import BaseModel, Field

import sandbox.api as sandbox_api
from sandbox.shared.models import Intent, PtyWriteRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.pty_command_tool import PtyCommandOutput, pty_tool_result
from tools.sandbox._lib.tool_context import (
    sandbox_caller_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class PtyWriteInput(BaseModel):
    pty_session_id: str = Field(..., min_length=1)
    chars: str = ""
    yield_time_ms: int = Field(default=1000, ge=0, le=30_000)
    max_tokens: int | None = Field(default=None, ge=1)


@tool(
    name="write_pty_command_stdin",
    description="Write literal text to an active PTY command session.",
    short_description="Write PTY input.",
    input_model=PtyWriteInput,
    output_model=PtyCommandOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def write_pty_command_stdin(
    pty_session_id: str,
    chars: str = "",
    yield_time_ms: int = 1000,
    max_tokens: int | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    result = await sandbox_api.write_pty_command_stdin(
        sandbox_id,
        PtyWriteRequest(
            caller=sandbox_caller_from_tool_context(context),
            pty_session_id=pty_session_id,
            chars=chars,
            yield_time_ms=yield_time_ms,
            max_tokens=max_tokens,
        ),
    )
    return pty_tool_result(result)


__all__ = ["write_pty_command_stdin"]
