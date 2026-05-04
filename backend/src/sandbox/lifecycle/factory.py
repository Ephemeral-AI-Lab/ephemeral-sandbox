"""Provider-neutral sandbox lifecycle factory (transitional shim).

This module is a deletion target — S5 (caller migration to
:mod:`sandbox.api.lifecycle`) wipes it. Lives here only so that
:mod:`server.routers.sandboxes`, :mod:`sandbox.testing.fixtures`, and
:mod:`benchmarks.sweevo.sandbox` keep compiling between commits.
"""

from __future__ import annotations

from typing import Any


def lifecycle_provider_for(*, provider: str = "daytona") -> Any:
    """Return the provider-owned lifecycle implementation."""
    if provider != "daytona":
        raise ValueError(f"Unsupported sandbox provider: {provider}")
    from sandbox.providers.daytona.lifecycle import DaytonaSandboxLifecycle

    return DaytonaSandboxLifecycle()


__all__ = ["lifecycle_provider_for"]
