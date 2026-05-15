"""OCC backend factory for daemon handlers and services.

This module owns the single OCC backend tuple consumed by every daemon peer
that needs layer-stack/OCC/gitignore state: handlers/request_context.py
(api.write/edit/read), service/shell_runner.py (api.shell), and
handlers/metrics.py (api.layer_metrics).
The factory uses a canonical ``workspace_ref=layer_stack_root`` only; this module
owns no path classification (single source of truth lives on command-exec
via :mod:`sandbox.daemon.handler.request_context`).
"""

from __future__ import annotations

import threading
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.client import OccClient
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle
from sandbox.occ.maintenance import AutoSquashMaintenancePolicy
from sandbox.occ.service import AUTO_SQUASH_MAX_DEPTH, OccService
from sandbox.daemon.service.layer_stack_client import LayerStackClient
from sandbox.daemon.service.workspace_binding import RuntimeWorkspaceBindingReader
from sandbox.daemon.service.workspace_server import get_layer_stack_manager


@dataclass(frozen=True)
class OccBackend:
    """The OCC backend tuple shared by every runtime peer.

    Field names are the structural contract: ``handler.request_context``,
    ``service.shell_runner``, and ``handler.metrics`` all read these
    attributes. A typo here silently breaks every consumer.
    """

    layer_stack: LayerStackClient
    occ_service: OccService
    occ_client: OccClient
    gitignore: SnapshotGitignoreOracle
    manager: LayerStackManager


_MAX_BACKEND_CACHE_ENTRIES = 256
_BACKEND_CACHE: OrderedDict[str, OccBackend] = OrderedDict()
_BACKEND_CACHE_LOCK = threading.RLock()


def build_occ_backend(layer_stack_root: str) -> OccBackend:
    """Return the cached OCC backend for ``layer_stack_root`` (constructing on miss)."""
    cache_key = _backend_cache_key(layer_stack_root)
    with _BACKEND_CACHE_LOCK:
        cached = _BACKEND_CACHE.get(cache_key)
        if cached is not None:
            _BACKEND_CACHE.move_to_end(cache_key)
            return cached
    manager = get_layer_stack_manager(cache_key)
    layer_stack = LayerStackClient(manager)
    gitignore = SnapshotGitignoreOracle(layer_stack)
    occ_service = OccService(
        gitignore=gitignore,
        layer_stack=layer_stack,
        maintenance=AutoSquashMaintenancePolicy(
            snapshot_reader=layer_stack,
            squasher=layer_stack,
            max_depth=AUTO_SQUASH_MAX_DEPTH,
        ),
    )
    occ_client = OccClient(
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
    with _BACKEND_CACHE_LOCK:
        existing = _BACKEND_CACHE.get(cache_key)
        if existing is not None:
            _BACKEND_CACHE.move_to_end(cache_key)
            return existing
        _BACKEND_CACHE[cache_key] = backend
        _evict_oldest_backends()
    return backend


def drop_backend_cache(layer_stack_root: str) -> None:
    """Drop cached OCC backend for one layer-stack root."""
    root = str(layer_stack_root or "").strip()
    if not root:
        return
    with _BACKEND_CACHE_LOCK:
        _BACKEND_CACHE.pop(str(Path(root).resolve(strict=False)), None)


def clear_backend_cache() -> None:
    """Drop every cached OCC backend. Test helper."""
    with _BACKEND_CACHE_LOCK:
        _BACKEND_CACHE.clear()


def _backend_cache_key(layer_stack_root: str | Path) -> str:
    raw = str(layer_stack_root or "").strip()
    if not raw:
        raise ValueError("layer_stack_root is required")
    return str(Path(raw).resolve(strict=False))


def _evict_oldest_backends() -> None:
    """Caller must hold ``_BACKEND_CACHE_LOCK``."""
    while len(_BACKEND_CACHE) > _MAX_BACKEND_CACHE_ENTRIES:
        _BACKEND_CACHE.popitem(last=False)


__all__ = [
    "OccBackend",
    "build_occ_backend",
    "clear_backend_cache",
    "drop_backend_cache",
]
