"""Process-local sandbox provider adapter registry.

Two registration modes coexist:

- :func:`set_default_provider` / :func:`get_default_provider` — the
  process-wide default provider used by ``list``/``health``/``create`` paths
  before a sandbox-id has been minted.
- :func:`register_adapter` / :func:`get_adapter` / :func:`dispose_adapter` —
  per-sandbox-id binding used by ``exec`` and instance-scoped operations. When
  a sandbox was discovered outside this process-local map, ``get_adapter``
  falls back to the default provider and caches that association.
"""

from __future__ import annotations

import threading

from sandbox.provider.protocol import ProviderAdapter

_ADAPTERS: dict[str, ProviderAdapter] = {}
_DEFAULT: ProviderAdapter | None = None
_LOCK = threading.Lock()


def set_default_provider(adapter: ProviderAdapter) -> None:
    """Bind the process-wide default provider adapter."""
    global _DEFAULT
    with _LOCK:
        _DEFAULT = adapter


def get_default_provider() -> ProviderAdapter:
    """Return the process-wide default provider adapter.

    Raises ``RuntimeError`` when none has been registered.
    """
    with _LOCK:
        if _DEFAULT is None:
            raise RuntimeError(
                "No default sandbox provider registered. "
                "Call set_default_provider(...) during app startup."
            )
        return _DEFAULT


def register_adapter(sandbox_id: str, adapter: ProviderAdapter) -> None:
    """Bind *sandbox_id* to *adapter* in this orchestrator process."""
    if not sandbox_id:
        raise ValueError("sandbox_id is required")
    with _LOCK:
        _ADAPTERS[sandbox_id] = adapter


def has_registered_adapter(sandbox_id: str) -> bool:
    """Return whether *sandbox_id* has an explicit process-local binding."""
    with _LOCK:
        return sandbox_id in _ADAPTERS


def get_adapter(sandbox_id: str) -> ProviderAdapter:
    """Return the provider adapter for *sandbox_id*.

    Raises ``KeyError`` when no adapter has been registered and no default
    provider exists.
    """
    if not sandbox_id:
        raise ValueError("sandbox_id is required")
    with _LOCK:
        adapter = _ADAPTERS.get(sandbox_id)
        if adapter is not None:
            return adapter
        if _DEFAULT is None:
            raise KeyError(sandbox_id)
        # WR-01: do NOT cache the fallback. Pre-fix this assigned
        # _ADAPTERS[sandbox_id] = _DEFAULT for any unknown id, growing
        # the cache without bound AND making has_registered_adapter
        # report True after the first fallback lookup — flipping
        # "explicit register" indistinguishable from "fallback cached".
        return _DEFAULT


def dispose_adapter(sandbox_id: str) -> None:
    """Remove the provider adapter for *sandbox_id* if present."""
    with _LOCK:
        _ADAPTERS.pop(sandbox_id, None)


__all__ = [
    "dispose_adapter",
    "get_adapter",
    "get_default_provider",
    "has_registered_adapter",
    "register_adapter",
    "set_default_provider",
]
