"""Sanitize daytona_shell commands that route output through pipes, redirects, or repo-root cd."""

from __future__ import annotations

import re
from dataclasses import dataclass

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit.hooks.prehook._shell_common import shell_command

PIPELINE_POLICY_MESSAGE = (
    "daytona_shell policy error: command could not be sanitized to a runnable command. "
    "Commands must not contain `|`, `>`, `2>&1`, `head`, `tail`, or a leading "
    "`cd /testbed &&` / `cd /workspace &&`. "
    "`daytona_shell` already captures stdout/stderr and starts at the repo root. "
    "Use pytest flags (`-x`, `-k`, `--tb=short`), a narrower node id, "
    "or background execution to limit output."
)

PIPELINE_POLICY_ADVISORY = (
    "sanitized daytona_shell command before execution; removed unsupported output "
    "piping/redirection, head/tail filtering, or a leading repo-root cd."
)

_LEADING_ROOT_CD_RE = re.compile(r"^\s*cd\s+/(testbed|workspace)\s*&&\s*", re.DOTALL)
_HEAD_TAIL_RE = re.compile(r"^\s*(head|tail)(?=\s|$)")
_HEAD_TAIL_OPTION_RE = re.compile(
    r"^\s*(?:-\d+|-[nc]\s+\S+|-[nc]\S+|--(?:lines|bytes)(?:=\S+|\s+\S+)?|-[qv])"
    r"(?=\s|$)"
)


@dataclass(frozen=True)
class _SanitizedCommand:
    command: str
    changed: bool
    error: str | None = None


def _is_word_char(char: str) -> bool:
    return char.isalnum() or char == "_"


def _previous_allows_redirection(command: str, index: int) -> bool:
    return index == 0 or not _is_word_char(command[index - 1])


def _skip_shell_word(command: str, index: int) -> int:
    quote: str | None = None
    escaped = False
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
        if char.isspace() or char in {";", "|", "&", "(", ")"}:
            break
        index += 1
    return index


def _skip_redirect_target(command: str, index: int) -> int:
    while index < len(command) and command[index].isspace():
        index += 1
    if index < len(command) and command[index] == "&":
        index += 1
        return _skip_shell_word(command, index)
    return _skip_shell_word(command, index)


def _redirection_span_at(command: str, index: int) -> tuple[int, int] | None:
    char = command[index]
    if char == "&" and index + 1 < len(command) and command[index + 1] == ">":
        op_end = index + 2
        if op_end < len(command) and command[op_end] == ">":
            op_end += 1
        return index, _skip_redirect_target(command, op_end)

    if char == ">":
        op_end = index + 1
        if op_end < len(command) and command[op_end] == ">":
            op_end += 1
        return index, _skip_redirect_target(command, op_end)

    if char.isdigit() and _previous_allows_redirection(command, index):
        fd_end = index + 1
        while fd_end < len(command) and command[fd_end].isdigit():
            fd_end += 1
        if fd_end < len(command) and command[fd_end] == ">":
            op_end = fd_end + 1
            if op_end < len(command) and command[op_end] == ">":
                op_end += 1
            return index, _skip_redirect_target(command, op_end)

    return None


def _strip_unquoted_redirections(command: str) -> tuple[str, bool]:
    changed = False
    out: list[str] = []
    quote: str | None = None
    escaped = False
    i = 0
    n = len(command or "")
    while i < n:
        char = command[i]
        if escaped:
            out.append(char)
            escaped = False
            i += 1
            continue
        if char == "\\":
            out.append(char)
            escaped = True
            i += 1
            continue
        if quote:
            out.append(char)
            if char == quote:
                quote = None
            i += 1
            continue
        if char in {"'", '"'}:
            out.append(char)
            quote = char
            i += 1
            continue
        span = _redirection_span_at(command, i)
        if span is not None:
            _, end = span
            changed = True
            i = max(end, i + 1)
            continue
        out.append(char)
        i += 1
    return "".join(out).strip(), changed


def _strip_after_unquoted_pipe(command: str) -> tuple[str, bool]:
    quote: str | None = None
    escaped = False
    i = 0
    while i < len(command):
        char = command[i]
        if escaped:
            escaped = False
            i += 1
            continue
        if char == "\\":
            escaped = True
            i += 1
            continue
        if quote:
            if char == quote:
                quote = None
            i += 1
            continue
        if char in {"'", '"'}:
            quote = char
            i += 1
            continue
        if char == "|":
            return command[:i].strip(), True
        i += 1
    return command, False


def _strip_leading_repo_root_cd(command: str) -> tuple[str, bool]:
    sanitized = _LEADING_ROOT_CD_RE.sub("", command or "", count=1)
    return sanitized.strip(), sanitized != command


def _strip_head_tail_options(rest: str) -> str:
    while True:
        match = _HEAD_TAIL_OPTION_RE.match(rest)
        if match is None:
            return rest.strip()
        rest = rest[match.end() :]


def _rewrite_head_tail_command(command: str) -> tuple[str, bool]:
    match = _HEAD_TAIL_RE.match(command or "")
    if match is None:
        return command, False
    rest = _strip_head_tail_options(command[match.end() :])
    if not rest:
        return "", True
    return f"cat {rest}", True


def _find_arithmetic_expansion_end(command: str, index: int) -> int | None:
    end = command.find("))", index + 3)
    return None if end < 0 else end + 1


def _find_command_substitution_end(command: str, body_start: int) -> int | None:
    quote: str | None = None
    escaped = False
    depth = 1
    i = body_start
    while i < len(command):
        char = command[i]
        if escaped:
            escaped = False
            i += 1
            continue
        if char == "\\":
            escaped = True
            i += 1
            continue
        if quote == "'":
            if char == quote:
                quote = None
            i += 1
            continue
        if quote == '"':
            if char == quote:
                quote = None
                i += 1
                continue
            if command[i : i + 3] == "$((":
                arithmetic_end = _find_arithmetic_expansion_end(command, i)
                if arithmetic_end is not None:
                    i = arithmetic_end + 1
                    continue
            if _starts_command_substitution(command, i):
                depth += 1
                i += 2
                continue
            i += 1
            continue
        if char in {"'", '"'}:
            quote = char
            i += 1
            continue
        if command[i : i + 3] == "$((":
            arithmetic_end = _find_arithmetic_expansion_end(command, i)
            if arithmetic_end is not None:
                i = arithmetic_end + 1
                continue
        if _starts_command_substitution(command, i):
            depth += 1
            i += 2
            continue
        if char == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def _starts_command_substitution(command: str, index: int) -> bool:
    return (
        command[index : index + 2] == "$("
        and command[index : index + 3] != "$(("
    )


def _sanitize_command_substitution_at(
    command: str,
    index: int,
) -> tuple[_SanitizedCommand | None, int]:
    end = _find_command_substitution_end(command, index + 2)
    if end is None:
        return None, index
    sanitized = _sanitize_shell_command(command[index + 2 : end])
    if sanitized.error is not None:
        return sanitized, end + 1
    return (
        _SanitizedCommand(f"$({sanitized.command})", changed=sanitized.changed),
        end + 1,
    )


def _sanitize_command_substitutions(command: str) -> _SanitizedCommand:
    out: list[str] = []
    changed = False
    quote: str | None = None
    escaped = False
    i = 0
    while i < len(command):
        char = command[i]
        if escaped:
            out.append(char)
            escaped = False
            i += 1
            continue
        if char == "\\":
            out.append(char)
            escaped = True
            i += 1
            continue
        if quote == "'":
            out.append(char)
            if char == quote:
                quote = None
            i += 1
            continue
        if quote == '"':
            if char == quote:
                out.append(char)
                quote = None
                i += 1
                continue
            if _starts_command_substitution(command, i):
                sanitized, next_index = _sanitize_command_substitution_at(command, i)
                if sanitized is not None:
                    if sanitized.error is not None:
                        return _SanitizedCommand(command, changed, sanitized.error)
                    out.append(sanitized.command)
                    changed = changed or sanitized.changed
                    i = next_index
                    continue
            out.append(char)
            i += 1
            continue
        if char in {"'", '"'}:
            out.append(char)
            quote = char
            i += 1
            continue
        if _starts_command_substitution(command, i):
            sanitized, next_index = _sanitize_command_substitution_at(command, i)
            if sanitized is not None:
                if sanitized.error is not None:
                    return _SanitizedCommand(command, changed, sanitized.error)
                out.append(sanitized.command)
                changed = changed or sanitized.changed
                i = next_index
                continue
        out.append(char)
        i += 1
    return _SanitizedCommand("".join(out), changed=changed)


def _collapse_unquoted_horizontal_space(command: str) -> str:
    out: list[str] = []
    quote: str | None = None
    escaped = False
    pending_space = False
    for char in command:
        if escaped:
            out.append(char)
            escaped = False
            pending_space = False
            continue
        if char == "\\":
            out.append(char)
            escaped = True
            pending_space = False
            continue
        if quote:
            out.append(char)
            if char == quote:
                quote = None
            continue
        if char in {"'", '"'}:
            out.append(char)
            quote = char
            pending_space = False
            continue
        if char in {" ", "\t"}:
            if not pending_space:
                out.append(" ")
                pending_space = True
            continue
        out.append(char)
        pending_space = False
    return "".join(out).strip()


def _sanitize_shell_command(command: str) -> _SanitizedCommand:
    sanitized = command or ""
    changed = False
    nested = _sanitize_command_substitutions(sanitized)
    if nested.error is not None:
        return nested
    sanitized = nested.command
    changed = changed or nested.changed
    for sanitizer in (
        _strip_leading_repo_root_cd,
        _strip_after_unquoted_pipe,
        _strip_unquoted_redirections,
        _rewrite_head_tail_command,
    ):
        sanitized, did_change = sanitizer(sanitized)
        changed = changed or did_change

    sanitized = sanitized.strip()
    if changed:
        sanitized = _collapse_unquoted_horizontal_space(sanitized)
    if changed and not sanitized:
        return _SanitizedCommand("", changed=True, error=PIPELINE_POLICY_MESSAGE)
    return _SanitizedCommand(sanitized, changed=changed)


def sanitize_shell_pipeline_policy(args: BaseModel) -> PreHookOutcome:
    command = shell_command(args)
    if command is None:
        return PreHookOutcome()
    sanitized = _sanitize_shell_command(command)
    if sanitized.error is not None:
        return PreHookOutcome(has_error=True, error_message=sanitized.error)
    if sanitized.changed:
        return PreHookOutcome(
            tool_input=args.model_copy(update={"command": sanitized.command}),
            advisories=(PIPELINE_POLICY_ADVISORY,),
        )
    return PreHookOutcome()


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del context
    return sanitize_shell_pipeline_policy(args)


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "pre",
        27,
        hook,
        name="daytona_shell:output_pipeline_policy",
    )
