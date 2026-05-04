"""Shell command routing policy for public sandbox shell."""

from __future__ import annotations

import re
import shlex

_SHELL_OPERATORS = frozenset(
    {"&&", "||", "&", "&>", ">&", "|&", ";", ">", ">>", "<", "<<", "(", ")"}
)
_ASSIGNMENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=.*$")
_READ_ONLY_COMMANDS = frozenset(
    {
        "basename",
        "cat",
        "cut",
        "date",
        "df",
        "dirname",
        "du",
        "egrep",
        "fgrep",
        "file",
        "grep",
        "head",
        "jq",
        "ls",
        "pwd",
        "readlink",
        "realpath",
        "rg",
        "stat",
        "tail",
        "tr",
        "wc",
        "which",
    }
)
_SORT_WRITE_OPTIONS = frozenset({"-o", "--output"})
_SED_WRITE_OPTIONS = frozenset({"-i", "--in-place"})
_FIND_WRITE_ACTIONS = frozenset(
    {
        "-delete",
        "-exec",
        "-execdir",
        "-fls",
        "-fprint",
        "-fprint0",
        "-fprintf",
        "-ok",
        "-okdir",
    }
)
_GIT_OPTIONS_WITH_VALUES = frozenset(
    {
        "-C",
        "-c",
        "--config-env",
        "--exec-path",
        "--git-dir",
        "--namespace",
        "--super-prefix",
        "--work-tree",
    }
)
_GIT_FLAG_OPTIONS = frozenset(
    {
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
)
_READ_ONLY_GIT_SUBCOMMANDS = frozenset(
    {
        "blame",
        "cat-file",
        "describe",
        "diff",
        "grep",
        "log",
        "ls-files",
        "rev-list",
        "rev-parse",
        "shortlog",
        "show",
        "show-ref",
        "status",
    }
)


def is_read_only_pipeline(command: str) -> bool:
    """Return whether *command* can bypass overlay/OCC through raw exec."""
    segments = _pipeline_segments(command)
    if segments is None:
        return False
    return all(_is_read_only_segment(segment) for segment in segments)


def _pipeline_segments(command: str) -> tuple[tuple[str, ...], ...] | None:
    if not command.strip() or "\n" in command or "\r" in command:
        return None
    try:
        lexer = shlex.shlex(
            command,
            posix=True,
            punctuation_chars="|&;()<>",
        )
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return None
    if not tokens:
        return None

    segments: list[tuple[str, ...]] = []
    current: list[str] = []
    for token in tokens:
        if token == "|":
            if not current:
                return None
            segments.append(tuple(current))
            current = []
            continue
        if token in _SHELL_OPERATORS or _unsafe_shell_token(token):
            return None
        current.append(token)
    if not current:
        return None
    segments.append(tuple(current))
    return tuple(segments)


def _unsafe_shell_token(token: str) -> bool:
    return (
        "`" in token
        or "$(" in token
        or "<(" in token
        or ">(" in token
        or token == "$"
    )


def _is_read_only_segment(segment: tuple[str, ...]) -> bool:
    tokens = _strip_env_prefix(segment)
    if not tokens:
        return False
    command = tokens[0]
    if command == "command":
        if len(tokens) >= 3 and tokens[1] == "-v":
            return True
        tokens = tokens[1:]
        if not tokens:
            return False
        command = tokens[0]
    if command == "git":
        return _git_segment_is_read_only(tokens[1:])
    if command == "find":
        return not any(_find_token_is_write_action(token) for token in tokens[1:])
    if command == "sort":
        return not _contains_option_with_value(tokens[1:], _SORT_WRITE_OPTIONS)
    if command == "sed":
        return not _contains_option_with_value(tokens[1:], _SED_WRITE_OPTIONS)
    if command == "uniq":
        return _uniq_segment_is_read_only(tokens[1:])
    return command in _READ_ONLY_COMMANDS


def _strip_env_prefix(segment: tuple[str, ...]) -> tuple[str, ...]:
    idx = 0
    while idx < len(segment) and _ASSIGNMENT_RE.match(segment[idx]):
        if _unsafe_shell_token(segment[idx]):
            return ()
        idx += 1
    return segment[idx:]


def _find_token_is_write_action(token: str) -> bool:
    return token in _FIND_WRITE_ACTIONS


def _contains_option_with_value(tokens: tuple[str, ...], options: frozenset[str]) -> bool:
    idx = 0
    while idx < len(tokens):
        token = tokens[idx]
        if token == "--":
            return False
        if token in options:
            return True
        if any(token.startswith(option) and token != "-" for option in options):
            return True
        if any(token.startswith(f"{option}=") for option in options):
            return True
        idx += 1
    return False


def _uniq_segment_is_read_only(tokens: tuple[str, ...]) -> bool:
    positional = tuple(token for token in tokens if not token.startswith("-"))
    return len(positional) <= 1


def _git_segment_is_read_only(args: tuple[str, ...]) -> bool:
    parsed = _git_subcommand(args)
    if parsed is None:
        return False
    subcommand, subcommand_args = parsed
    if subcommand in _READ_ONLY_GIT_SUBCOMMANDS:
        return True
    if subcommand == "apply":
        return (
            "--check" in subcommand_args
            and "--cached" not in subcommand_args
            and "--index" not in subcommand_args
        )
    if subcommand == "clean":
        return _git_clean_is_dry_run(subcommand_args)
    if subcommand == "config":
        return _git_config_is_read_only(subcommand_args)
    if subcommand == "branch":
        return _git_branch_is_read_only(subcommand_args)
    return False


def _git_subcommand(args: tuple[str, ...]) -> tuple[str, tuple[str, ...]] | None:
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


def _git_clean_is_dry_run(args: tuple[str, ...]) -> bool:
    for arg in args:
        if arg == "--":
            return False
        if arg == "--dry-run":
            return True
        if arg.startswith("--"):
            continue
        if arg.startswith("-") and "n" in arg[1:]:
            return True
    return False


def _git_config_is_read_only(args: tuple[str, ...]) -> bool:
    return any(
        arg in {"--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list", "-l"}
        for arg in args
    )


def _git_branch_is_read_only(args: tuple[str, ...]) -> bool:
    positional = tuple(arg for arg in args if not arg.startswith("-"))
    return not positional


__all__ = ["is_read_only_pipeline"]
