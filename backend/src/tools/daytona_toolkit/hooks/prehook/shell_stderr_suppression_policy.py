"""Block daytona_shell commands that hide stderr from runtime evidence."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit.hooks.prehook._shell_common import (
    shell_commands,
)

STDERR_SUPPRESSION_POLICY_MESSAGE = (
    "daytona_shell policy error: daytona_shell commands must preserve stderr. "
    "Do not suppress stderr with `2>/dev/null`, `&>/dev/null`, or "
    "`>/dev/null 2>&1`; `daytona_shell` already captures stdout and stderr."
)


def _is_word_char(char: str) -> bool:
    return char.isalnum() or char == "_"


def _previous_allows_redirection(command: str, index: int) -> bool:
    return index == 0 or not _is_word_char(command[index - 1])


def _read_shell_word(command: str, index: int) -> tuple[str, int]:
    out: list[str] = []
    quote: str | None = None
    escaped = False
    while index < len(command):
        char = command[index]
        if escaped:
            out.append(char)
            escaped = False
            index += 1
            continue
        if char == "\\":
            escaped = True
            index += 1
            continue
        if quote:
            if char == quote:
                quote = None
            else:
                out.append(char)
            index += 1
            continue
        if char in {"'", '"'}:
            quote = char
            index += 1
            continue
        if char.isspace() or char in {";", "|", "&", ")"}:
            break
        out.append(char)
        index += 1
    return "".join(out), index


def _skip_redirect_operator(command: str, index: int) -> int:
    if index < len(command) and command[index] == ">":
        index += 1
    return index


def _read_redirect_target(command: str, index: int) -> tuple[str, int]:
    while index < len(command) and command[index].isspace():
        index += 1
    return _read_shell_word(command, index)


def _has_stderr_suppression(command: str) -> bool:
    quote: str | None = None
    escaped = False
    stdout_to_dev_null = False
    index = 0
    while index < len(command):
        char = command[index]
        if escaped:
            escaped = False
            index += 1
            continue
        if char == "\\":
            escaped = True
            index += 1
            continue
        if quote:
            if char == quote:
                quote = None
            index += 1
            continue
        if char in {"'", '"'}:
            quote = char
            index += 1
            continue
        if char in {";", "|", ")"} or (
            char == "&" and command[index : index + 2] != "&>"
        ):
            stdout_to_dev_null = False
            index += 1
            continue
        if (
            char == "&"
            and index + 1 < len(command)
            and command[index + 1] == ">"
        ):
            target, next_index = _read_redirect_target(
                command, _skip_redirect_operator(command, index + 2)
            )
            if target == "/dev/null":
                return True
            index = max(next_index, index + 2)
            continue

        start = index
        fd: int | None = None
        if char.isdigit() and _previous_allows_redirection(command, index):
            while index < len(command) and command[index].isdigit():
                index += 1
            if index < len(command) and command[index] == ">":
                fd = int(command[start:index])
            else:
                index = start + 1
                continue
        elif char == ">":
            fd = 1
        else:
            index += 1
            continue

        if index >= len(command) or command[index] != ">":
            index += 1
            continue
        index += 1
        index = _skip_redirect_operator(command, index)
        if index < len(command) and command[index] == "&":
            target, next_index = _read_shell_word(command, index + 1)
            if fd == 2 and target == "-":
                return True
            if fd == 2 and target == "1" and stdout_to_dev_null:
                return True
            index = max(next_index, index + 1)
            continue

        target, next_index = _read_redirect_target(command, index)
        if fd == 2 and target == "/dev/null":
            return True
        if fd == 1 and target == "/dev/null":
            stdout_to_dev_null = True
        index = max(next_index, index + 1)
    return False


def shell_stderr_suppression_policy_error(args: BaseModel) -> str | None:
    for command in shell_commands(args):
        if _has_stderr_suppression(command):
            return STDERR_SUPPRESSION_POLICY_MESSAGE
    return None


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del context
    err = shell_stderr_suppression_policy_error(args)
    if err is not None:
        return PreHookOutcome(has_error=True, error_message=err)
    return PreHookOutcome()


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "pre",
        28,
        hook,
        name="daytona_shell:stderr_suppression_policy",
    )
