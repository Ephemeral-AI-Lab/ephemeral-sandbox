"""IntelligenceQueryRouter — priority-based multi-backend query routing.

Routes queries by descending priority across registered backends
(LSP > SymbolIndex). Fallback only on ``unsupported`` or ``unavailable``,
never on error. Authoritative empty answers stop the search.
"""

from __future__ import annotations

import logging
from typing import Any

from ephemeralos.services.code_intelligence.backend_protocol import (
    BackendQueryOutcome,
    CodeIntelligenceBackend,
    QueryStatus,
)
from ephemeralos.services.code_intelligence.types import (
    Diagnostic,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)

logger = logging.getLogger(__name__)

# Statuses that trigger fallback to next backend
_FALLBACK_STATUSES = {QueryStatus.UNSUPPORTED, QueryStatus.UNAVAILABLE}


class IntelligenceQueryRouter:
    """Routes CI queries across multiple backends by priority.

    Backends are tried in descending priority order. A backend's empty
    result is authoritative (no further fallback). Only ``unsupported``
    or ``unavailable`` triggers fallback.
    """

    def __init__(self) -> None:
        self._backends: list[CodeIntelligenceBackend] = []

    def register(self, backend: CodeIntelligenceBackend) -> None:
        """Register a query backend."""
        self._backends.append(backend)
        self._backends.sort(key=lambda b: b.priority, reverse=True)
        logger.debug(
            "Registered backend %s (priority=%d)", backend.name, backend.priority,
        )

    def find_definitions(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[SymbolInfo]:
        """Find symbol definitions, routing through backends."""
        for backend in self._backends:
            if not backend.supports(file_path):
                continue
            outcome = backend.find_definitions(file_path, symbol, line, character)
            if outcome.status in _FALLBACK_STATUSES:
                continue
            if outcome.status == QueryStatus.ERROR:
                logger.warning("Backend %s error: %s", backend.name, outcome.error)
                return []
            return outcome.results or []
        return []

    def find_references(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[ReferenceInfo]:
        """Find references, routing through backends."""
        for backend in self._backends:
            if not backend.supports(file_path):
                continue
            outcome = backend.find_references(file_path, symbol, line, character)
            if outcome.status in _FALLBACK_STATUSES:
                continue
            if outcome.status == QueryStatus.ERROR:
                logger.warning("Backend %s error: %s", backend.name, outcome.error)
                return []
            return outcome.results or []
        return []

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        """Get hover information, routing through backends."""
        for backend in self._backends:
            if not backend.supports(file_path):
                continue
            outcome = backend.hover(file_path, line, character)
            if outcome.status in _FALLBACK_STATUSES:
                continue
            if outcome.status == QueryStatus.ERROR:
                logger.warning("Backend %s error: %s", backend.name, outcome.error)
                return None
            results = outcome.results or []
            return results[0] if results else None
        return None

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Get diagnostics, routing through backends."""
        for backend in self._backends:
            if not backend.supports(file_path):
                continue
            outcome = backend.diagnostics(file_path)
            if outcome.status in _FALLBACK_STATUSES:
                continue
            if outcome.status == QueryStatus.ERROR:
                logger.warning("Backend %s error: %s", backend.name, outcome.error)
                return []
            return outcome.results or []
        return []

    def register_file_change(self, file_path: str) -> None:
        """Notify backends of a file change for cache invalidation."""
        for backend in self._backends:
            invalidate = getattr(backend, "invalidate", None)
            if callable(invalidate):
                try:
                    invalidate(file_path)
                except Exception:
                    pass

    @property
    def backend_names(self) -> list[str]:
        return [b.name for b in self._backends]
