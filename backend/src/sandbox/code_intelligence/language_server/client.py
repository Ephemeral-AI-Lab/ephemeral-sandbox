"""Semantic language-server-backed code intelligence queries."""

from __future__ import annotations

import threading
from collections import OrderedDict
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any, TypeVar

from sandbox.code_intelligence.core.constants import (
    LSP_CACHE_MAX_ENTRIES,
    LSP_CACHE_TTL,
)
from sandbox.code_intelligence.core.types import (
    Diagnostic,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)
from sandbox.code_intelligence.language_server.cache import LspCacheMixin
from sandbox.code_intelligence.language_server.models import (
    LspTelemetry,
    _CacheEntry,
    _InflightQuery,
)
from sandbox.code_intelligence.language_server.python_backend import PythonBackendMixin
from sandbox.code_intelligence.language_server.telemetry import LspTelemetryMixin
from sandbox.code_intelligence.language_server.transport import LspTransportMixin
from sandbox.code_intelligence.language_server.utils import _readiness_targets

_T = TypeVar("_T")


class LspClient(PythonBackendMixin, LspTransportMixin, LspCacheMixin, LspTelemetryMixin):
    """Hybrid semantic backend with subprocess queries and caching."""

    def __init__(
        self,
        workspace_root: str = "",
        sandbox: Any = None,
        cache_ttl: float = LSP_CACHE_TTL,
        cache_max: int = LSP_CACHE_MAX_ENTRIES,
    ) -> None:
        self._workspace_root = workspace_root
        self._sandbox = sandbox
        self._cache_ttl = cache_ttl
        self._cache_max = cache_max

        self._cache_lock = threading.Lock()
        self._counter_lock = threading.Lock()
        self._line_cache_lock = threading.Lock()

        self._cache: OrderedDict[str, _CacheEntry] = OrderedDict()
        self._inflight: dict[str, _InflightQuery] = {}
        self._line_cache: OrderedDict[tuple[str, int], str | None] = OrderedDict()
        self._telemetry = LspTelemetry()
        self._py_available: bool | None = None

    # -- Public query methods -------------------------------------------------

    def goto_definition(
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> list[SymbolInfo]:
        """Find symbol definitions at position."""
        return self._run_cached_query(
            f"def:{file_path}:{line}:{character}",
            lambda: self._query_python(
                file_path,
                lambda: self._python_definitions(file_path, line, character),
                [],
            ),
        )

    def find_references(
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> list[ReferenceInfo]:
        """Find all references to symbol at position."""
        return self._run_cached_query(
            f"ref:{file_path}:{line}:{character}",
            lambda: self._query_python(
                file_path,
                lambda: self._python_references(file_path, line, character),
                [],
            ),
        )

    def hover(
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> HoverResult | None:
        """Get hover information at position."""
        return self._run_cached_query(
            f"hover:{file_path}:{line}:{character}",
            lambda: self._query_python(
                file_path,
                lambda: self._python_hover(file_path, line, character),
                None,
            ),
            cache_when=lambda result: result is not None,
        )

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Get diagnostics for a file."""
        return self._run_cached_query(
            f"diag:{file_path}",
            lambda: self._query_python(
                file_path,
                lambda: self._python_diagnostics(file_path),
                [],
            ),
        )

    def invalidate(self, file_path: str) -> None:
        """Invalidate all cached results for a file."""
        resolved_path = self._resolve_path(file_path)
        candidates = {str(file_path), resolved_path}
        if self._workspace_root:
            try:
                relative_path = Path(resolved_path).relative_to(self._workspace_root)
            except ValueError:
                pass
            else:
                candidates.add(str(relative_path))
                candidates.add(relative_path.as_posix())

        with self._cache_lock:
            to_remove = [
                k
                for k in self._cache
                if any(candidate and candidate in k for candidate in candidates)
            ]
            for k in to_remove:
                del self._cache[k]
        with self._line_cache_lock:
            stale = [key for key in self._line_cache if key[0] == resolved_path]
            for key in stale:
                del self._line_cache[key]

    def close(self) -> None:
        """Release LSP resources."""
        return None

    def ensure_ready(
        self,
        *,
        install_missing: bool = False,
        languages: Sequence[str] | None = None,
    ) -> dict[str, bool]:
        """Check which language backends are available.

        When attached to a sandbox, optionally install bounded missing
        dependencies so CI can recover from a cold image. ``languages``
        scopes the probe to supported Python backends.
        """
        targets = _readiness_targets(languages)
        if "python" in targets and self._py_available is None:
            self._py_available = self._check_python_backend()
        if install_missing and self._sandbox:
            if "python" in targets and not self._py_available:
                self._py_available = self._install_python_backend()
        return {"python": self._py_available or False} if "python" in targets else {}

    def reset_backend_availability(self) -> None:
        """Forget cached backend readiness so the next probe can re-check."""
        self._py_available = None

    @property
    def telemetry(self) -> LspTelemetry:
        with self._counter_lock:
            return LspTelemetry(
                queries=self._telemetry.queries,
                errors=self._telemetry.errors,
                successes=self._telemetry.successes,
                cache_hits=self._telemetry.cache_hits,
                script_runs=self._telemetry.script_runs,
                script_successes=self._telemetry.script_successes,
                script_errors=self._telemetry.script_errors,
            )

    @property
    def connected(self) -> bool:
        """Whether at least one language backend is available."""
        status = self.ensure_ready(languages=("python",))
        return bool(status.get("python"))

    def _query_python(
        self,
        file_path: str,
        loader: Callable[[], _T],
        empty: _T,
    ) -> _T:
        self._record_query()
        if self._detect_language(file_path) != "python":
            return empty
        return loader()
