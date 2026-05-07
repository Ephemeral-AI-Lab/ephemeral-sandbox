"""Context passed to tool factories during agent construction."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ToolFactoryContext:
    """Runtime context passed to tool factories during agent construction."""

    metadata: dict[str, Any] = field(default_factory=dict)


__all__ = ["ToolFactoryContext"]
