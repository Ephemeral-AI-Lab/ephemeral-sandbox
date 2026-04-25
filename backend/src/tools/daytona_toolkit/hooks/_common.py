"""Shared helpers for Daytona platform hook modules."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import (
    _get_cwd,
    _resolve_path,
)


def str_arg(args: BaseModel, name: str) -> str | None:
    value = getattr(args, name, None)
    return value if isinstance(value, str) and value else None


def resolved_arg(args: BaseModel, name: str, context: ToolExecutionContext) -> str | None:
    value = str_arg(args, name)
    return _resolve_path(value, context) if value is not None else None


__all__ = [
    "_get_cwd",
    "_resolve_path",
    "resolved_arg",
    "str_arg",
]
