"""Global per-sandbox runtime service registry."""

from __future__ import annotations

import threading
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from sandbox.api.transport import SandboxTransport
    from sandbox.runtime.service import CodeIntelligenceService

_SERVICES: dict[str, "CodeIntelligenceService"] = {}
_SERVICES_LOCK = threading.Lock()
_CREATION_LOCKS: dict[str, threading.Lock] = {}


def get_code_intelligence(
    sandbox_id: str,
    workspace_root: str = "/workspace",
    sandbox: Any = None,
    *,
    transport: "SandboxTransport | None" = None,
) -> "CodeIntelligenceService":
    """Get or create a runtime service for *sandbox_id*.

    When ``transport`` is supplied, downstream subsystems route sandbox I/O
    through the provider-neutral transport. Defaulting to ``None`` preserves
    local/test callers that construct the service with only ``sandbox=``.
    """
    from sandbox.runtime.service import CodeIntelligenceService

    def _transport_matches(service: CodeIntelligenceService) -> bool:
        current = getattr(service, "_transport", None)
        if transport is None:
            return current is None
        return current is transport

    with _SERVICES_LOCK:
        existing = _SERVICES.get(sandbox_id)
        if (
            existing is not None
            and existing.workspace_root == workspace_root
            and _transport_matches(existing)
        ):
            existing.rebind_sandbox(sandbox)
            return existing
        if sandbox_id not in _CREATION_LOCKS:
            _CREATION_LOCKS[sandbox_id] = threading.Lock()
        creation_lock = _CREATION_LOCKS[sandbox_id]

    with creation_lock:
        with _SERVICES_LOCK:
            existing = _SERVICES.get(sandbox_id)
            if (
                existing is not None
                and existing.workspace_root == workspace_root
                and _transport_matches(existing)
            ):
                existing.rebind_sandbox(sandbox)
                return existing
            if existing is not None:
                _SERVICES.pop(sandbox_id, None)

        if existing is not None:
            existing.dispose()

        service = CodeIntelligenceService(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            sandbox=sandbox,
            transport=transport,
        )
        with _SERVICES_LOCK:
            _SERVICES[sandbox_id] = service
        return service


def get_code_intelligence_if_exists(sandbox_id: str) -> "CodeIntelligenceService | None":
    """Fetch an existing runtime service without creating one."""
    with _SERVICES_LOCK:
        return _SERVICES.get(sandbox_id)


def dispose_code_intelligence(sandbox_id: str) -> None:
    """Dispose and remove a CI service."""
    with _SERVICES_LOCK:
        service = _SERVICES.pop(sandbox_id, None)
    if service:
        service.dispose()


def dispose_all_code_intelligence() -> None:
    """Dispose all runtime services."""
    with _SERVICES_LOCK:
        services = list(_SERVICES.values())
        _SERVICES.clear()
    for service in services:
        service.dispose()
