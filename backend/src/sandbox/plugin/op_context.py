"""``PluginOpContext`` passed to in-sandbox plugin op handlers."""

from __future__ import annotations

from dataclasses import dataclass, field
from contextlib import AbstractAsyncContextManager
from typing import Any, Protocol

from sandbox._shared.models import SandboxCaller

__all__ = [
    "PluginOpContext",
    "SandboxOverlayLike",
    "ProjectionHandleLike",
    "WorkspaceProjectionLike",
]


class ProjectionHandleLike(Protocol):
    """Minimal protocol every projection handle satisfies."""

    manifest_key: str
    lowerdir: str | None
    lease_id: str
    layer_paths: tuple[str, ...] | None

    def release(self) -> None: ...


class WorkspaceProjectionLike(Protocol):
    """Minimal protocol every workspace projection satisfies."""

    @property
    def layer_stack_root(self) -> Any: ...

    def acquire(
        self,
        owner_request_id: str,
        *,
        lowerdir_root: str | None = None,
        materialize: bool = True,
    ) -> ProjectionHandleLike: ...

    def acquire_overlay(
        self,
        owner_request_id: str,
        *,
        workspace_root: str,
        materialize: bool = False,
    ) -> ProjectionHandleLike: ...

    def active_manifest_key(self) -> str: ...


class SandboxOverlayLike(Protocol):
    """Minimal daemon overlay surface exposed to plugin tool calls."""

    @property
    def workspace_root(self) -> str: ...

    def active_manifest_key(self) -> str: ...

    async def ensure_current(self, *, reason: str = "ensure_current") -> str: ...

    def current_manifest(self) -> Any: ...

    def acquire_operation_overlay(
        self,
        *,
        request_id: str,
        workspace_root: str | None = None,
        materialize: bool = False,
    ) -> Any: ...

    def release_operation_overlay(self, handle: Any) -> None: ...

    def workspace_operation(
        self,
        *,
        reason: str = "operation",
    ) -> AbstractAsyncContextManager[Any]: ...

    async def publish_workspace_paths(
        self,
        *,
        paths: list[str] | tuple[str, ...],
        actor_id: str = "",
        description: str = "plugin workspace edit",
    ) -> object: ...

    async def publish_cycle(
        self,
        *,
        request: Any,
        upperdir: str,
        snapshot: Any,
        run_maintenance: bool = True,
    ) -> object: ...


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
    overlay: SandboxOverlayLike
    metadata: dict[str, Any] = field(default_factory=dict)
