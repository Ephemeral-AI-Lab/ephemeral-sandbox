"""Backend protocol for sandbox runtime clients."""

from __future__ import annotations

from typing import Any, Protocol


class CodeIntelligenceBackend(Protocol):
    """Shape that every code-intelligence runtime backend implements."""

    sandbox_id: str
    workspace_root: str
    is_initialized: bool

    def ensure_initialized(self, wait: bool = True) -> bool: ...
    def warmup(self) -> None: ...
    def rebind_sandbox(self, sandbox: Any) -> None: ...
    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any: ...
    def dispose(self) -> None: ...


__all__ = ["CodeIntelligenceBackend"]
