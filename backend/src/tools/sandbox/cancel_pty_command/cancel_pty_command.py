"""Cancel an active PTY command."""

from __future__ import annotations

from pydantic import BaseModel, Field

import sandbox.api as sandbox_api
from sandbox.shared.models import Intent, PtyCancelRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.pty_command_tool import PtyCommandOutput, pty_tool_result
from tools.sandbox._lib.tool_context import (
    sandbox_caller_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class PtyCancelInput(BaseModel):
    pty_session_id: str = Field(..., min_length=1)


@tool(
    name="cancel_pty_command",
    description="Cancel an active PTY command session.",
    short_description="Cancel PTY command.",
    input_model=PtyCancelInput,
    output_model=PtyCommandOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def cancel_pty_command(
    pty_session_id: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    result = await sandbox_api.cancel_pty_command(
        sandbox_id,
        PtyCancelRequest(
            caller=sandbox_caller_from_tool_context(context),
            pty_session_id=pty_session_id,
        ),
    )
    manager = context.get("background_task_manager")
    mark = getattr(manager, "mark_pty_cancelled_by_tool", None)
    if callable(mark):
        mark(pty_session_id)
    return pty_tool_result(result)


__all__ = ["cancel_pty_command"]
