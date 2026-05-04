"""Per-sandbox runtime service facade.

The facade delegates every public op to the provider-backed runtime backend.
Constructing a runtime service without a registered provider adapter is a
configuration error; the old in-process fallback is intentionally gone.

After the OCC simplification this surface is intentionally minimal:
mutation requests flow through typed OCC services, not through service-level
write/edit methods or runtime OCC wire handlers.
"""

from __future__ import annotations

from typing import Any

from sandbox.providers.registry import get_adapter
from sandbox.runtime.backends import (
    CodeIntelligenceBackend,
    DaemonBackend,
)

__all__ = ["CodeIntelligenceService"]

def _select_backend(
    sandbox_id: str,
    workspace_root: str,
) -> CodeIntelligenceBackend:
    """Create the provider-backed backend, failing closed without an adapter."""
    _require_provider_adapter(sandbox_id)
    return DaemonBackend(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
    )


def _require_provider_adapter(sandbox_id: str) -> None:
    if not sandbox_id:
        raise ValueError("sandbox_id is required for sandbox runtime services")
    try:
        get_adapter(sandbox_id)
    except KeyError as exc:
        raise RuntimeError(
            f"Provider adapter is required for sandbox runtime service {sandbox_id!r}"
        ) from exc


class CodeIntelligenceService:
    """Thin facade that forwards every public op to the selected backend."""

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
    ) -> None:
        del sandbox
        self._impl: CodeIntelligenceBackend = _select_backend(
            sandbox_id,
            workspace_root,
        )

    @property
    def sandbox_id(self) -> str:
        return self._impl.sandbox_id

    @property
    def workspace_root(self) -> str:
        return self._impl.workspace_root

    @property
    def is_initialized(self) -> bool:
        return self._impl.is_initialized

    def ensure_initialized(self, wait: bool = True) -> bool:
        return self._impl.ensure_initialized(wait=wait)

    def warmup(self) -> None:
        self._impl.warmup()

    def rebind_sandbox(self, sandbox: Any) -> None:
        self._impl.rebind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._impl.cmd(sandbox, command, **kwargs)

    def dispose(self) -> None:
        self._impl.dispose()
