"""Write bytes to or poll an active command session."""

from __future__ import annotations

from pydantic import BaseModel, Field

import sandbox.api as sandbox_api
from sandbox._shared.models import CommandSessionCancelRequest, CommandSessionWriteRequest, Intent
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.command_session_tool import (
    CommandToolOutput,
    command_tool_result,
    mark_command_session_result_reported_by_tool,
    recover_command_session_result_from_supervisor,
)
from tools.sandbox._lib.tool_context import (
    sandbox_caller_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class WriteStdinInput(BaseModel):
    command_session_id: str = Field(..., min_length=1)
    chars: str = ""
    yield_time_ms: int = Field(default=1000, ge=0, le=30_000)
    max_output_tokens: int | None = Field(default=None, ge=1)


@tool(
    name="write_stdin",
    description="Write literal text to an active command session, or poll with empty input.",
    short_description="Write session input.",
    input_model=WriteStdinInput,
    output_model=CommandToolOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def write_stdin(
    command_session_id: str,
    chars: str = "",
    yield_time_ms: int = 1000,
    max_output_tokens: int | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    result = await sandbox_api.write_stdin(
        sandbox_id,
        CommandSessionWriteRequest(
            caller=sandbox_caller_from_tool_context(context),
            command_session_id=command_session_id,
            chars=chars,
            yield_time_ms=yield_time_ms,
            max_output_tokens=max_output_tokens,
        ),
    )
    caller = sandbox_caller_from_tool_context(context)
    if "\x03" in chars and result.status == "running":
        result = await sandbox_api.cancel_command_session(
            sandbox_id,
            CommandSessionCancelRequest(
                caller=caller,
                command_session_id=command_session_id,
            ),
        )
    result = recover_command_session_result_from_supervisor(
        context,
        result,
        command_session_id=command_session_id,
    )
    mark_command_session_result_reported_by_tool(
        context,
        result,
        command_session_id=command_session_id,
    )
    return command_tool_result(result)


__all__ = ["write_stdin"]
