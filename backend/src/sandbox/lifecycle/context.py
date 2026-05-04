"""Provider-neutral sandbox context-preparer interface."""

from __future__ import annotations

from sandbox.providers.protocol import SandboxContextPreparer


def context_preparer_for(
    sandbox_id: str,
    *,
    provider: str = "daytona",
) -> SandboxContextPreparer:
    """Return the provider-owned context preparer for *sandbox_id*."""
    if provider != "daytona":
        raise ValueError(f"Unsupported sandbox provider: {provider}")
    from sandbox.providers.daytona.context import DaytonaContextPreparer

    return DaytonaContextPreparer(sandbox_id)


__all__ = [
    "SandboxContextPreparer",
    "context_preparer_for",
]
