"""Block git mutation commands in daytona_shell shell mode."""

from __future__ import annotations

import re
import shlex

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit.hooks.prehook._shell_common import shell_commands

_GIT_COMMAND_PATTERN = re.compile(
    r"(?:^|[;&|]\s*)(?:command\s+)?git\b(?P<args>[^;&|]*)",
    flags=re.IGNORECASE,
)
_GIT_OPTIONS_WITH_VALUES = {
    "-C",
    "-c",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--namespace",
    "--super-prefix",
    "--work-tree",
}
_GIT_FLAG_OPTIONS = {
    "--bare",
    "--glob-pathspecs",
    "--icase-pathspecs",
    "--literal-pathspecs",
    "--no-pager",
    "--no-replace-objects",
    "--noglob-pathspecs",
    "--paginate",
    "-P",
    "-p",
}
_BLOCKED_GIT_SUBCOMMANDS = {
    "add",
    "am",
    "checkout",
    "checkout-index",
    "cherry-pick",
    "commit",
    "merge",
    "mv",
    "read-tree",
    "rebase",
    "reset",
    "restore",
    "revert",
    "rm",
    "stash",
    "switch",
    "update-index",
}
_DESTRUCTIVE_GIT_MESSAGE = (
    "BLOCKED: daytona_shell is for runtime commands, tests, and inspection. "
    "destructive git commands and other git mutation commands are forbidden. "
    "Detected filesystem mutation command or git metadata mutation. They mutate "
    "repository metadata or working-tree files outside the OCC/write-scope audit "
    "path. Use daytona_edit_file, daytona_write_file, daytona_delete_file, "
    "or daytona_move_file instead."
)


def _clean_args_are_dry_run(args: list[str]) -> bool:
    for arg in args:
        if arg == "--":
            break
        if arg == "--dry-run":
            return True
        if arg.startswith("--"):
            continue
        if arg.startswith("-") and "n" in arg[1:]:
            return True
    return False


def _split_git_args(raw_args: str) -> list[str]:
    try:
        return shlex.split(raw_args)
    except ValueError:
        return raw_args.split()


def _git_subcommand(args: list[str]) -> tuple[str, list[str]] | None:
    idx = 0
    while idx < len(args):
        arg = args[idx]
        if arg == "--":
            return None
        if arg in _GIT_OPTIONS_WITH_VALUES:
            idx += 2
            continue
        if any(arg.startswith(f"{option}=") for option in _GIT_OPTIONS_WITH_VALUES):
            idx += 1
            continue
        if arg in _GIT_FLAG_OPTIONS:
            idx += 1
            continue
        if arg.startswith("-"):
            idx += 1
            continue
        return arg.lower(), args[idx + 1 :]
    return None


def _git_apply_is_read_only(args: list[str]) -> bool:
    return "--check" in args and "--cached" not in args and "--index" not in args


def _has_git_mutation_command(command: str) -> bool:
    for match in _GIT_COMMAND_PATTERN.finditer(command or ""):
        parsed = _git_subcommand(_split_git_args(match.group("args") or ""))
        if parsed is None:
            continue
        subcommand, args = parsed
        if subcommand == "clean":
            if not _clean_args_are_dry_run(args):
                return True
            continue
        if subcommand == "apply":
            if not _git_apply_is_read_only(args):
                return True
            continue
        if subcommand in _BLOCKED_GIT_SUBCOMMANDS:
            return True
    return False


def destructive_git_command_error(command: str) -> str | None:
    if _has_git_mutation_command(command):
        return _DESTRUCTIVE_GIT_MESSAGE
    return None


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del context
    for command in shell_commands(args):
        err = destructive_git_command_error(command)
        if err is not None:
            return PreHookOutcome(has_error=True, error_message=err)
    return PreHookOutcome()


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "pre",
        10,
        hook,
        name="daytona_shell:destructive_git",
    )
