"""Shared helpers for daytona_shell pre-hooks."""

from __future__ import annotations

from pydantic import BaseModel


def shell_command(args: BaseModel) -> str | None:
    command = getattr(args, "command", None)
    if not isinstance(command, str) or not command.strip():
        return None
    return command


def shell_commands(args: BaseModel) -> list[str]:
    command = shell_command(args)
    return [command] if command is not None else []
