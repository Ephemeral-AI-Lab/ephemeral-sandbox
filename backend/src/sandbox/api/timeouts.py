"""Timeout policy for public sandbox API verbs."""

from __future__ import annotations

READ_FILE_TIMEOUT_S = 60
WRITE_FILE_TIMEOUT_S = 60
EDIT_FILE_TIMEOUT_S = 20
SHELL_DEFAULT_COMMAND_TIMEOUT_S = 60
SHELL_DISPATCH_GRACE_S = 30
GLOB_TIMEOUT_S = 60
GREP_TIMEOUT_S = 60


def shell_dispatch_timeout(command_timeout_s: int | None) -> int:
    command_budget = (
        SHELL_DEFAULT_COMMAND_TIMEOUT_S
        if command_timeout_s is None
        else command_timeout_s
    )
    return command_budget + SHELL_DISPATCH_GRACE_S


__all__ = [
    "EDIT_FILE_TIMEOUT_S",
    "GLOB_TIMEOUT_S",
    "GREP_TIMEOUT_S",
    "READ_FILE_TIMEOUT_S",
    "SHELL_DEFAULT_COMMAND_TIMEOUT_S",
    "SHELL_DISPATCH_GRACE_S",
    "WRITE_FILE_TIMEOUT_S",
    "shell_dispatch_timeout",
]
