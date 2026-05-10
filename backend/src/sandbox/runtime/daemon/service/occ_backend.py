"""OCC backend factory for daemon handlers and services.

This module owns the single OCC backend tuple consumed by every daemon peer
that needs layer-stack/OCC/gitignore state: handlers/request_context.py
(api.write/edit/read), service/shell_runner.py (api.shell via
handler/tools/shell.py), and handlers/metrics.py (api.layer_metrics).
The factory uses a canonical ``workspace_ref=layer_stack_root`` only; this module
owns no path classification (single source of truth lives on command-exec
via :mod:`sandbox.runtime.daemon.handler.request_context`).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.client import OCCClient
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle
from sandbox.occ.service import AUTO_SQUASH_DRAIN_TIMEOUT_S, OccService
from sandbox.runtime.daemon.service.layer_stack_client import LayerStackClient
from sandbox.runtime.daemon.service.workspace_binding import RuntimeWorkspaceBindingReader
from sandbox.runtime.daemon.service.workspace_server import get_layer_stack_manager


@dataclass(frozen=True)
class OccBackend:
    """The OCC backend tuple shared by every runtime peer.

    Field names are the structural contract: ``handler.request_context``,
    ``service.shell_runner``, and ``handler.metrics`` all read these
    attributes. A typo here silently breaks every consumer.
    """

    layer_stack: LayerStackClient
    occ_service: OccService
    occ_client: OCCClient
    gitignore: SnapshotGitignoreOracle
    manager: LayerStackManager


_BACKEND_CACHE: dict[str, OccBackend] = {}


def build_occ_backend(layer_stack_root: str) -> OccBackend:
    """Return the cached OCC backend for ``layer_stack_root`` (constructing on miss)."""
    cache_key = _backend_cache_key(layer_stack_root)
    cached = _BACKEND_CACHE.get(cache_key)
    if cached is not None:
        return cached
    manager = get_layer_stack_manager(cache_key)
    layer_stack = LayerStackClient(manager)
    gitignore = SnapshotGitignoreOracle(layer_stack)
    occ_service = OccService(gitignore=gitignore, layer_stack=layer_stack)
    occ_client = OCCClient(
        occ_service,
        binding_reader=RuntimeWorkspaceBindingReader(),
        workspace_ref=cache_key,
    )
    backend = OccBackend(
        layer_stack=layer_stack,
        occ_service=occ_service,
        occ_client=occ_client,
        gitignore=gitignore,
        manager=manager,
    )
    _BACKEND_CACHE[cache_key] = backend
    return backend


def drop_backend_cache(layer_stack_root: str) -> None:
    """Drop cached OCC backend for one layer-stack root."""
    root = str(layer_stack_root or "").strip()
    if not root:
        return
    _BACKEND_CACHE.pop(root, None)
    _BACKEND_CACHE.pop(str(Path(root).resolve(strict=False)), None)


async def drain_backend_auto_squash(
    layer_stack_root: str,
    *,
    timeout_s: float = AUTO_SQUASH_DRAIN_TIMEOUT_S,
) -> dict[str, object] | None:
    """Drain pending async auto-squash work for a cached backend."""
    root = str(layer_stack_root or "").strip()
    if not root:
        return None
    backend = _BACKEND_CACHE.get(root)
    if backend is None:
        backend = _BACKEND_CACHE.get(str(Path(root).resolve(strict=False)))
    if backend is None:
        return None
    return await backend.occ_service.drain_auto_squash_maintenance(
        timeout_s=timeout_s,
    )


def _backend_cache_clear() -> None:
    """Drop every cached OCC backend. Test helper."""
    _BACKEND_CACHE.clear()


def _backend_cache_key(layer_stack_root: str | Path) -> str:
    raw = str(layer_stack_root or "").strip()
    if not raw:
        raise ValueError("layer_stack_root is required")
    return str(Path(raw).resolve(strict=False))


__all__ = [
    "OccBackend",
    "build_occ_backend",
    "drain_backend_auto_squash",
    "drop_backend_cache",
]
