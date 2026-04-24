"""Daytona tool package."""

from __future__ import annotations

# Side-effect import: registers Daytona platform hooks on the default hook
# registry whenever the package loads.
from tools.daytona_toolkit import hooks as _hooks  # noqa: F401
from tools.daytona_toolkit.context import DaytonaContextPreparer
from tools.daytona_toolkit.toolkit import make_daytona_tools

__all__ = ["DaytonaContextPreparer", "make_daytona_tools"]
