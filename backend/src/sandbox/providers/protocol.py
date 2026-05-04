"""Provider-neutral sandbox execution adapter."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from sandbox.api.utils.models import RawExecResult


class ProviderAdapter(Protocol):
    """Minimal provider primitive used by raw runtime/setup paths."""

    name: str

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult: ...


__all__ = ["ProviderAdapter"]
