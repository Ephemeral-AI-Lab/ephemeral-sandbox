"""Daytona toolkit package."""

from __future__ import annotations

from typing import Any

# Side-effect import: registers the write-scope and CodeAct pre-phase tool
# guards on the default tool-guard registry. Imported here so guards are
# active whenever any module in the daytona_toolkit package loads (tests
# and the toolkit loader both trigger this path).
from tools.daytona_toolkit import guards as _guards  # noqa: F401

__all__ = ["DaytonaToolkit"]


def __getattr__(name: str) -> Any:
    if name == "DaytonaToolkit":
        from tools.daytona_toolkit.toolkit import DaytonaToolkit

        return DaytonaToolkit
    raise AttributeError(name)
