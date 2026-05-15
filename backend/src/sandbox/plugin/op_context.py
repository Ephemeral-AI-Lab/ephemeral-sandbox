"""``PluginOpContext`` passed to in-sandbox plugin op handlers."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol

from sandbox._shared.models import SandboxCaller

__all__ = [
    "PluginOpContext",
    "ProjectionHandleLike",
    "WorkspaceProjectionLike",
]


class ProjectionHandleLike(Protocol):
    """Minimal protocol every projection handle satisfies."""

    manifest_key: str
    lowerdir: str
    lease_id: str

    def release(self) -> None: ...


class WorkspaceProjectionLike(Protocol):
    """Minimal protocol every workspace projection satisfies."""

    @property
    def layer_stack_root(self) -> Any: ...

    def acquire(self, owner_request_id: str) -> ProjectionHandleLike: ...

    def active_manifest_key(self) -> str: ...


@dataclass(frozen=True)
class PluginOpContext:
    """Concrete context surface a plugin op handler may rely on.

    Plugin authors MUST NOT import sandbox.* directly; they receive everything
    they need through this dataclass. The host wires up ``projection`` using
    the real :mod:`sandbox.plugin.projection`; tests inject a stub
    duck-typed object.
    """

    layer_stack_root: str
    caller: SandboxCaller
    projection: WorkspaceProjectionLike
    metadata: dict[str, Any] = field(default_factory=dict)
