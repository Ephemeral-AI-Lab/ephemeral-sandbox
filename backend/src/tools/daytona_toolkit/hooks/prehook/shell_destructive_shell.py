"""Block destructive shell commands in daytona_shell shell mode."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit.ci_integration import destructive_shell_command_error
from tools.daytona_toolkit.hooks.prehook._shell_common import shell_commands


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del context
    for command in shell_commands(args):
        err = destructive_shell_command_error(command)
        if err is not None:
            return PreHookOutcome(has_error=True, error_message=err)
    return PreHookOutcome()


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "pre",
        20,
        hook,
        name="daytona_shell:destructive_shell",
    )
