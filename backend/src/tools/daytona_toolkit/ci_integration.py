"""Daytona-specific CI integration helpers."""

from __future__ import annotations

import re

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


def destructive_shell_command_error(command: str) -> str | None:
    """Return an error for always-blocked destructive shell commands."""
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        return (
            "BLOCKED: destructive shell command that targets workspace or system "
            "directories (rm -r /testbed, mv /testbed, etc.) is forbidden. "
            "These commands destroy the shared workspace and cannot be undone. "
            "Use targeted file operations instead."
        )
    return None


__all__ = [
    "destructive_shell_command_error",
]
