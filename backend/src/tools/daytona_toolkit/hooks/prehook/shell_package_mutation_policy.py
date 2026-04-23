"""Block package and environment mutation commands in daytona_shell."""

from __future__ import annotations

import re
import shlex

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry
from tools.daytona_toolkit.hooks.prehook._shell_common import shell_commands

_CONTROL_TOKENS = {";", "&&", "||", "|", "&", "(", ")"}
_BLOCKED_PAIRS = {
    ("apt", "install"),
    ("conda", "install"),
    ("npm", "install"),
    ("pnpm", "add"),
    ("poetry", "add"),
    ("uv", "add"),
    ("uv", "sync"),
    ("yarn", "add"),
}
_PIP_COMMAND_RE = re.compile(r"^pip(?:[0-9.]+)?$", flags=re.IGNORECASE)
_PYTHON_COMMAND_RE = re.compile(r"^python(?:[0-9.]+)?$", flags=re.IGNORECASE)
_PYTHON_OPTIONS_WITH_VALUES = {"-W", "-X"}
_ENV_OPTIONS_WITH_VALUES = {"-u", "--unset"}
_BLOCKED_PACKAGE_MUTATION_MESSAGE = (
    "daytona_shell policy error: package and environment mutation commands are "
    "forbidden. Blocked `{command}`. Do not install, add, sync, update, or "
    "upgrade dependencies from daytona_shell; capture the command/error evidence and "
    "request replanning instead."
)


def _shell_tokens(command: str) -> list[str]:
    lexer = shlex.shlex(command or "", posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    lexer.commenters = ""
    try:
        return list(lexer)
    except ValueError:
        try:
            return shlex.split(command or "", posix=False)
        except ValueError:
            return (command or "").split()


def _segments(tokens: list[str]) -> list[list[str]]:
    segments: list[list[str]] = []
    current: list[str] = []
    for token in tokens:
        if token in _CONTROL_TOKENS:
            if current:
                segments.append(current)
                current = []
            continue
        current.append(token)
    if current:
        segments.append(current)
    return segments


def _strip_prefix_options_and_assignments(tokens: list[str]) -> list[str]:
    idx = 0
    while idx < len(tokens):
        token = tokens[idx]
        if token == "--":
            idx += 1
            break
        if token.startswith("-"):
            idx += 1
            continue
        if "=" in token and not token.startswith("="):
            idx += 1
            continue
        break
    return tokens[idx:]


def _strip_env_prefix(tokens: list[str]) -> list[str]:
    idx = 0
    while idx < len(tokens):
        token = tokens[idx]
        if token == "--":
            idx += 1
            break
        if token in {"-i", "--ignore-environment"}:
            idx += 1
            continue
        if token in _ENV_OPTIONS_WITH_VALUES:
            idx += 2
            continue
        if token.startswith("--unset="):
            idx += 1
            continue
        if "=" in token and not token.startswith("="):
            idx += 1
            continue
        break
    return tokens[idx:]


def _strip_wrappers(tokens: list[str]) -> list[str]:
    rest = tokens
    changed = True
    while changed and rest:
        changed = False
        lowered = rest[0].lower()
        if lowered in {"command", "builtin", "exec"}:
            rest = rest[1:]
            changed = True
        elif lowered == "sudo":
            rest = _strip_prefix_options_and_assignments(rest[1:])
            changed = True
        elif lowered == "time":
            rest = _strip_prefix_options_and_assignments(rest[1:])
            changed = True
        elif lowered == "env":
            rest = _strip_env_prefix(rest[1:])
            changed = True
        elif lowered == "uv" and len(rest) >= 2 and rest[1].lower() == "run":
            rest = rest[2:]
            changed = True
    return rest


def _python_module_args(tokens: list[str]) -> list[str] | None:
    if not tokens or _PYTHON_COMMAND_RE.match(tokens[0]) is None:
        return None

    idx = 1
    while idx < len(tokens):
        token = tokens[idx]
        if token == "-m":
            return tokens[idx + 1 :]
        if token == "--":
            return None
        if token == "-c" or token.startswith("-c"):
            return None
        if token in _PYTHON_OPTIONS_WITH_VALUES:
            idx += 2
            continue
        if any(token.startswith(f"{option}") for option in _PYTHON_OPTIONS_WITH_VALUES):
            idx += 1
            continue
        if token.startswith("-"):
            idx += 1
            continue
        return None
    return None


def _blocked_invocation(tokens: list[str]) -> str | None:
    if len(tokens) < 2:
        return None

    lowered = [token.lower() for token in tokens]
    first = lowered[0]
    second = lowered[1]

    if _PIP_COMMAND_RE.match(tokens[0]) and second == "install":
        return "pip install"

    module_args = _python_module_args(tokens)
    if module_args is not None and len(module_args) >= 2:
        module_lowered = [token.lower() for token in module_args]
        if module_lowered[0] == "pip" and module_lowered[1] == "install":
            return "pip install"

    pair = (first, second)
    if pair in _BLOCKED_PAIRS:
        return f"{pair[0]} {pair[1]}"

    return None


def package_mutation_command_error(command: str) -> str | None:
    """Return a policy error if ``command`` mutates package/environment state."""

    for segment in _segments(_shell_tokens(command)):
        stripped = _strip_wrappers(segment)
        blocked = _blocked_invocation(stripped)
        if blocked is not None:
            return _BLOCKED_PACKAGE_MUTATION_MESSAGE.format(command=blocked)
    return None


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del context
    for command in shell_commands(args):
        err = package_mutation_command_error(command)
        if err is not None:
            return PreHookOutcome(has_error=True, error_message=err)
    return PreHookOutcome()


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "daytona_shell",
        "pre",
        24,
        hook,
        name="daytona_shell:package_mutation_policy",
    )
