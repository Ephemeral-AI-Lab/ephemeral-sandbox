"""LspClient — semantic language server queries.

Provides one-shot helper script queries for Python (jedi) and
TypeScript (TS compiler). Includes query caching with TTL and
dependency state tracking.

Lock ordering (Group C):
    C2: cache lock  <  C3: counter lock
"""

from __future__ import annotations

import json
import logging
import subprocess
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ephemeralos.services.code_intelligence.constants import (
    LSP_CACHE_MAX_ENTRIES,
    LSP_CACHE_TTL,
    LSP_QUERY_TIMEOUT,
)
from ephemeralos.services.code_intelligence.types import (
    Diagnostic,
    DiagnosticSeverity,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
    SymbolKind,
)

logger = logging.getLogger(__name__)


@dataclass
class _CacheEntry:
    """Cached LSP query result."""

    result: Any
    expires_at: float


@dataclass
class LspTelemetry:
    """LSP client telemetry."""

    queries: int = 0
    errors: int = 0
    successes: int = 0
    cache_hits: int = 0


class LspClient:
    """Hybrid semantic backend for code intelligence queries.

    Uses subprocess calls to language-specific tools (jedi for Python,
    tsc for TypeScript) with result caching.

    Parameters
    ----------
    workspace_root:
        Root directory for resolving paths.
    sandbox:
        Optional Daytona sandbox object for remote execution.
    cache_ttl:
        Cache TTL in seconds.
    cache_max:
        Maximum cache entries.
    """

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

        # Group C locks
        self._cache_lock = threading.Lock()  # C2
        self._counter_lock = threading.Lock()  # C3

        self._cache: OrderedDict[str, _CacheEntry] = OrderedDict()
        self._telemetry = LspTelemetry()
        self._py_available: bool | None = None
        self._ts_available: bool | None = None

    # -- Public query methods -------------------------------------------------

    def goto_definition(
        self, file_path: str, line: int, character: int,
    ) -> list[SymbolInfo]:
        """Find symbol definitions at position."""
        cache_key = f"def:{file_path}:{line}:{character}"
        cached = self._get_cached(cache_key)
        if cached is not None:
            return cached

        language = self._detect_language(file_path)
        results = self._query_definitions(file_path, line, character, language)
        self._put_cached(cache_key, results)
        return results

    def find_references(
        self, file_path: str, line: int, character: int,
    ) -> list[ReferenceInfo]:
        """Find all references to symbol at position."""
        cache_key = f"ref:{file_path}:{line}:{character}"
        cached = self._get_cached(cache_key)
        if cached is not None:
            return cached

        language = self._detect_language(file_path)
        results = self._query_references(file_path, line, character, language)
        self._put_cached(cache_key, results)
        return results

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        """Get hover information at position."""
        cache_key = f"hover:{file_path}:{line}:{character}"
        cached = self._get_cached(cache_key)
        if cached is not None:
            return cached

        language = self._detect_language(file_path)
        result = self._query_hover(file_path, line, character, language)
        if result:
            self._put_cached(cache_key, result)
        return result

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Get diagnostics for a file."""
        cache_key = f"diag:{file_path}"
        cached = self._get_cached(cache_key)
        if cached is not None:
            return cached

        language = self._detect_language(file_path)
        results = self._query_diagnostics(file_path, language)
        self._put_cached(cache_key, results)
        return results

    def invalidate(self, file_path: str) -> None:
        """Invalidate all cached results for a file."""
        with self._cache_lock:
            to_remove = [k for k in self._cache if file_path in k]
            for k in to_remove:
                del self._cache[k]

    def ensure_ready(self) -> dict[str, bool]:
        """Check which language backends are available."""
        if self._py_available is None:
            self._py_available = self._check_python_backend()
        if self._ts_available is None:
            self._ts_available = self._check_typescript_backend()
        return {"python": self._py_available or False, "typescript": self._ts_available or False}

    def mark_backend_unavailable(self, language: str) -> None:
        """Mark a backend as unavailable."""
        if language == "python":
            self._py_available = False
        elif language in ("typescript", "javascript"):
            self._ts_available = False

    @property
    def telemetry(self) -> LspTelemetry:
        with self._counter_lock:
            return LspTelemetry(
                queries=self._telemetry.queries,
                errors=self._telemetry.errors,
                successes=self._telemetry.successes,
                cache_hits=self._telemetry.cache_hits,
            )

    @property
    def connected(self) -> bool:
        """Whether at least one language backend is available."""
        status = self.ensure_ready()
        return any(status.values())

    # -- Language-specific queries --------------------------------------------

    def _query_definitions(
        self, file_path: str, line: int, character: int, language: str,
    ) -> list[SymbolInfo]:
        """Query definitions using language-specific backend."""
        self._record_query()
        if language == "python":
            return self._python_definitions(file_path, line, character)
        return []

    def _query_references(
        self, file_path: str, line: int, character: int, language: str,
    ) -> list[ReferenceInfo]:
        self._record_query()
        if language == "python":
            return self._python_references(file_path, line, character)
        return []

    def _query_hover(
        self, file_path: str, line: int, character: int, language: str,
    ) -> HoverResult | None:
        self._record_query()
        if language == "python":
            return self._python_hover(file_path, line, character)
        return None

    def _query_diagnostics(
        self, file_path: str, language: str,
    ) -> list[Diagnostic]:
        self._record_query()
        if language == "python":
            return self._python_diagnostics(file_path)
        return []

    # -- Python backend (jedi) ------------------------------------------------

    def _python_definitions(
        self, file_path: str, line: int, character: int,
    ) -> list[SymbolInfo]:
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{file_path}')\n"
            f"defs = s.goto(line={line}, column={character})\n"
            f"print(json.dumps([{{'name': d.name, 'path': str(d.module_path or ''), "
            f"'line': d.line or 0, 'col': d.column or 0, "
            f"'type': d.type}} for d in defs]))"
        )
        output = self._run_python_script(script)
        if not output:
            return []
        try:
            raw = json.loads(output)
            return [
                SymbolInfo(
                    name=d["name"],
                    kind=SymbolKind(d.get("type", "unknown")) if d.get("type") in SymbolKind.__members__.values() else SymbolKind.UNKNOWN,
                    file_path=d.get("path", ""),
                    line=d.get("line", 0),
                    character=d.get("col", 0),
                )
                for d in raw
            ]
        except (json.JSONDecodeError, KeyError):
            return []

    def _python_references(
        self, file_path: str, line: int, character: int,
    ) -> list[ReferenceInfo]:
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{file_path}')\n"
            f"refs = s.get_references(line={line}, column={character})\n"
            f"print(json.dumps([{{'path': str(r.module_path or ''), "
            f"'line': r.line or 0, 'col': r.column or 0}} for r in refs]))"
        )
        output = self._run_python_script(script)
        if not output:
            return []
        try:
            raw = json.loads(output)
            return [
                ReferenceInfo(
                    file_path=r.get("path", ""),
                    line=r.get("line", 0),
                    character=r.get("col", 0),
                )
                for r in raw
            ]
        except (json.JSONDecodeError, KeyError):
            return []

    def _python_hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{file_path}')\n"
            f"names = s.help(line={line}, column={character})\n"
            f"if names:\n"
            f"    n = names[0]\n"
            f"    sigs = s.get_signatures(line={line}, column={character})\n"
            f"    sig = str(sigs[0]) if sigs else ''\n"
            f"    print(json.dumps({{'name': n.name, 'type': n.type, "
            f"'docstring': (n.docstring() or '')[:500], 'signature': sig}}))\n"
            f"else:\n"
            f"    print('null')"
        )
        output = self._run_python_script(script)
        if not output or output.strip() == "null":
            return None
        try:
            raw = json.loads(output)
            return HoverResult(
                content=raw.get("docstring", ""),
                language="python",
            )
        except (json.JSONDecodeError, KeyError):
            return None

    def _python_diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Check Python syntax."""
        try:
            content = Path(file_path).read_text(encoding="utf-8")
            compile(content, file_path, "exec")
            return []
        except SyntaxError as e:
            return [
                Diagnostic(
                    file_path=file_path,
                    line=e.lineno or 0,
                    character=(e.offset or 1) - 1,
                    severity=DiagnosticSeverity.ERROR,
                    message=str(e.msg),
                    source="python",
                )
            ]

    # -- Script execution -----------------------------------------------------

    def _run_python_script(self, script: str) -> str:
        """Run a Python script locally or in the sandbox."""
        try:
            if self._sandbox:
                response = self._sandbox.process.exec(
                    f"python3 -c {repr(script)}",
                    timeout=int(LSP_QUERY_TIMEOUT),
                )
                result = response.result or ""
            else:
                proc = subprocess.run(
                    ["python3", "-c", script],
                    capture_output=True,
                    text=True,
                    timeout=LSP_QUERY_TIMEOUT,
                    cwd=self._workspace_root or None,
                )
                result = proc.stdout
            self._record_success()
            return result.strip()
        except Exception as e:
            self._record_error()
            logger.debug("LSP Python query failed: %s", e)
            return ""

    # -- Backend availability -------------------------------------------------

    def _check_python_backend(self) -> bool:
        try:
            if self._sandbox:
                resp = self._sandbox.process.exec("python3 -c 'import jedi'", timeout=10)
                return getattr(resp, "exit_code", 1) == 0
            proc = subprocess.run(
                ["python3", "-c", "import jedi"],
                capture_output=True, timeout=10,
            )
            return proc.returncode == 0
        except Exception:
            return False

    def _check_typescript_backend(self) -> bool:
        try:
            if self._sandbox:
                resp = self._sandbox.process.exec("npx tsc --version", timeout=10)
                return getattr(resp, "exit_code", 1) == 0
            proc = subprocess.run(
                ["npx", "tsc", "--version"],
                capture_output=True, timeout=10,
            )
            return proc.returncode == 0
        except Exception:
            return False

    # -- Cache ----------------------------------------------------------------

    def _get_cached(self, key: str) -> Any:
        with self._cache_lock:
            entry = self._cache.get(key)
            if entry and entry.expires_at > time.time():
                self._cache.move_to_end(key)
                with self._counter_lock:
                    self._telemetry.cache_hits += 1
                return entry.result
            if entry:
                del self._cache[key]
        return None

    def _put_cached(self, key: str, result: Any) -> None:
        with self._cache_lock:
            self._cache[key] = _CacheEntry(
                result=result,
                expires_at=time.time() + self._cache_ttl,
            )
            self._cache.move_to_end(key)
            while len(self._cache) > self._cache_max:
                self._cache.popitem(last=False)

    # -- Telemetry ------------------------------------------------------------

    def _record_query(self) -> None:
        with self._counter_lock:
            self._telemetry.queries += 1

    def _record_success(self) -> None:
        with self._counter_lock:
            self._telemetry.successes += 1

    def _record_error(self) -> None:
        with self._counter_lock:
            self._telemetry.errors += 1

    def _detect_language(self, file_path: str) -> str:
        ext = Path(file_path).suffix.lower()
        return {
            ".py": "python",
            ".js": "javascript",
            ".jsx": "javascript",
            ".ts": "typescript",
            ".tsx": "typescript",
        }.get(ext, "unknown")
