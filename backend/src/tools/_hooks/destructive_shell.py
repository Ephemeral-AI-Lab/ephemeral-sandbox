"""Prehooks for sandbox shell command policy."""

from __future__ import annotations

import re
import shlex

from pydantic import BaseModel

from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.hooks import HookResult

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
    "branch",
    "checkout",
    "checkout-index",
    "cherry-pick",
    "commit",
    "merge",
    "mv",
    "notes",
    "prune",
    "read-tree",
    "rebase",
    "replace",
    "reset",
    "restore",
    "revert",
    "rm",
    "stash",
    "submodule",
    "switch",
    "tag",
    "update-index",
    "update-ref",
    "worktree",
}
_DESTRUCTIVE_GIT_MESSAGE = (
    "BLOCKED: shell is for runtime commands, tests, and inspection. "
    "Destructive git mutation commands are forbidden here. "
    "They mutate repository metadata or working-tree files outside the "
    "OCC/write-scope audit path. Use edit_file or write_file instead. "
    "(Note: shell-substitution forms such as $(...), backticks, bash -c, "
    "or eval can bypass this prehook; the sandbox commit/write audit "
    "remains the authoritative isolation boundary.)"
)
_DESTRUCTIVE_SHELL_PATTERN = re.compile(
    r"(?:^|[;&|]\s*)(?:"
    r"rm\s+(?:-\S*[rR]\S*\s+|--recursive\s+)(?:/(?:testbed|workspace|home|opt|usr|var|etc|tmp)\b|/\s|/\.\.|\.\.)"
    r"|mv\s+/(?:testbed|workspace|home|opt|usr|var|etc)(?:/[^/\s]*)?(?:\s|$)"
    r"|chmod\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|chown\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|rm\s+-\S*[rR]\S*\s+\.\s*$"
    r"|mkfs\b|dd\s+.*of=/"
    r")",
    flags=re.IGNORECASE,
)
_DESTRUCTIVE_SHELL_MESSAGE = (
    "BLOCKED: destructive shell command that targets workspace or system "
    "directories (rm -r /testbed, mv /testbed, etc.) is forbidden. "
    "These commands destroy the shared workspace and cannot be undone. "
    "Use targeted file operations instead."
)


def _shell_command(args: BaseModel) -> str | None:
    command = getattr(args, "command", None)
    if not isinstance(command, str) or not command.strip():
        return None
    return command


# git clean short flags that may legitimately appear in a combined bundle
# alongside -n. Any short flag containing a character outside this set is
# treated as ambiguous and NOT a dry-run (fail closed). `-e` is excluded
# because it takes an argument and cannot be combined.
_GIT_CLEAN_SHORT_FLAGS: frozenset[str] = frozenset("ndfxXqi")


def _clean_args_are_dry_run(args: list[str]) -> bool:
    for arg in args:
        if arg == "--":
            break
        if arg == "--dry-run":
            return True
        if arg.startswith("--"):
            continue
        if arg.startswith("-") and len(arg) > 1:
            # Combined short flags: each char is its own flag. Treat as a
            # dry-run only if `-n` is present AND every char in the bundle
            # is a known git clean short flag. Anything else (e.g. `-nx`
            # where `x` is fine, `-ny` where `y` is unknown, `-an` where
            # `a` is unknown) is rejected — fail closed.
            chars = set(arg[1:])
            if "n" in chars and chars <= _GIT_CLEAN_SHORT_FLAGS:
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
    """Return an error for shell commands that mutate git state."""
    if _has_git_mutation_command(command):
        return _DESTRUCTIVE_GIT_MESSAGE
    return None


def destructive_shell_command_error(command: str) -> str | None:
    """Return an error for always-blocked destructive shell commands."""
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        return _DESTRUCTIVE_SHELL_MESSAGE
    return None


class DestructiveGitShellPreHook:
    """Block git working-tree or metadata mutations before shell execution."""

    name = "sandbox_shell:destructive_git"
    target_tool = "shell"

    async def run(
        self,
        tool_input: BaseModel,
        context: ToolExecutionContextService,
    ) -> HookResult[BaseModel]:
        del context
        command = _shell_command(tool_input)
        if command is None:
            return HookResult.pass_(tool_input)
        err = destructive_git_command_error(command)
        if err is not None:
            return HookResult.fail(err, metadata={"policy": "destructive_git"})
        return HookResult.pass_(tool_input)


class DestructiveShellPreHook:
    """Block destructive filesystem commands before shell execution."""

    name = "sandbox_shell:destructive_shell"
    target_tool = "shell"

    async def run(
        self,
        tool_input: BaseModel,
        context: ToolExecutionContextService,
    ) -> HookResult[BaseModel]:
        del context
        command = _shell_command(tool_input)
        if command is None:
            return HookResult.pass_(tool_input)
        err = destructive_shell_command_error(command)
        if err is not None:
            return HookResult.fail(err, metadata={"policy": "destructive_shell"})
        return HookResult.pass_(tool_input)


__all__ = [
    "DestructiveGitShellPreHook",
    "DestructiveShellPreHook",
    "destructive_git_command_error",
    "destructive_shell_command_error",
]
