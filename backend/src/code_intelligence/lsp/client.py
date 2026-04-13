"""Semantic language-server-backed code intelligence queries."""

from __future__ import annotations

import json
import logging
import subprocess
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from code_intelligence.constants import (
    LSP_CACHE_MAX_ENTRIES,
    LSP_CACHE_TTL,
    LSP_QUERY_TIMEOUT,
)
from code_intelligence.types import (
    Diagnostic,
    DiagnosticSeverity,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
    SymbolKind,
)

logger = logging.getLogger(__name__)

_LANGUAGE_BY_EXTENSION = {
    ".py": "python",
    ".js": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".tsx": "typescript",
}


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

        self._cache: OrderedDict[str, _CacheEntry] = OrderedDict()
        self._telemetry = LspTelemetry()
        self._py_available: bool | None = None
        self._ts_available: bool | None = None

    # -- Public query methods -------------------------------------------------

    def goto_definition(
        self, file_path: str, line: int, character: int,
    ) -> list[SymbolInfo]:
        """Find symbol definitions at position."""
        language = self._detect_language(file_path)
        return self._run_cached_query(
            f"def:{file_path}:{line}:{character}",
            lambda: self._query_definitions(file_path, line, character, language),
        )

    def find_references(
        self, file_path: str, line: int, character: int,
    ) -> list[ReferenceInfo]:
        """Find all references to symbol at position."""
        language = self._detect_language(file_path)
        return self._run_cached_query(
            f"ref:{file_path}:{line}:{character}",
            lambda: self._query_references(file_path, line, character, language),
        )

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        """Get hover information at position."""
        language = self._detect_language(file_path)
        return self._run_cached_query(
            f"hover:{file_path}:{line}:{character}",
            lambda: self._query_hover(file_path, line, character, language),
            cache_when=lambda result: result is not None,
        )

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Get diagnostics for a file."""
        language = self._detect_language(file_path)
        return self._run_cached_query(
            f"diag:{file_path}",
            lambda: self._query_diagnostics(file_path, language),
        )

    def invalidate(self, file_path: str) -> None:
        """Invalidate all cached results for a file."""
        with self._cache_lock:
            to_remove = [k for k in self._cache if file_path in k]
            for k in to_remove:
                del self._cache[k]

    def ensure_ready(self, *, install_missing: bool = False) -> dict[str, bool]:
        """Check which language backends are available.

        When attached to a sandbox, optionally install bounded missing
        dependencies so CI can recover from a cold image.
        """
        if self._py_available is None:
            self._py_available = self._check_python_backend()
        if self._ts_available is None:
            self._ts_available = self._check_typescript_backend()
        if install_missing and self._sandbox:
            if not self._py_available:
                self._py_available = self._install_python_backend()
            if not self._ts_available:
                self._ts_available = self._install_typescript_backend()
        return {"python": self._py_available or False, "typescript": self._ts_available or False}

    def reset_backend_availability(self) -> None:
        """Forget cached backend readiness so the next probe can re-check."""
        self._py_available = None
        self._ts_available = None

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

    def _resolve_path(self, file_path: str) -> str:
        """Resolve a potentially relative file path against workspace root."""
        p = Path(file_path)
        if not p.is_absolute() and self._workspace_root:
            p = Path(self._workspace_root) / p
        return str(p)

    def _resolve_column(self, file_path: str, line: int, character: int) -> int:
        """When character is 0, advance to the first non-whitespace column.

        Jedi's ``help()``, ``get_references()``, and ``goto()`` need the
        cursor on the actual symbol text.  Callers (ci_hover,
        ci_query_references) often pass ``character=0`` which lands on
        leading indentation — producing empty results.

        Returns the resolved column (0-indexed).
        """
        if character != 0:
            return character
        try:
            abs_path = self._resolve_path(file_path)
            if self._sandbox:
                resp = self._resolve(
                    self._sandbox.process.exec(
                        f"sed -n '{line}p' {abs_path!r}",
                        timeout=5,
                    )
                )
                text = str(getattr(resp, "result", "") or "")
            else:
                p = Path(abs_path)
                if not p.exists():
                    return 0
                lines = p.read_text(encoding="utf-8").splitlines()
                if line < 1 or line > len(lines):
                    return 0
                text = lines[line - 1]
            stripped = text.lstrip()
            if not stripped:
                return 0
            return len(text) - len(stripped)
        except Exception:
            logger.debug("_resolve_column failed for %s:%d", file_path, line)
            return 0

    def _python_definitions(
        self, file_path: str, line: int, character: int,
    ) -> list[SymbolInfo]:
        character = self._resolve_column(file_path, line, character)
        abs_path = self._resolve_path(file_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{abs_path}')\n"
            f"defs = s.goto(line={line}, column={character})\n"
            f"print(json.dumps([{{'name': d.name, 'path': str(d.module_path or ''), "
            f"'line': d.line or 0, 'col': d.column or 0, "
            f"'type': d.type}} for d in defs]))"
        )
        output = self._run_python_script(script)
        raw = self._decode_json(output)
        if not isinstance(raw, list):
            return []
        return [
            SymbolInfo(
                name=str(item.get("name", "")),
                kind=_coerce_symbol_kind(item.get("type")),
                file_path=str(item.get("path", "")),
                line=int(item.get("line", 0) or 0),
                character=int(item.get("col", 0) or 0),
            )
            for item in raw
            if isinstance(item, dict) and item.get("name")
        ]

    def _python_references(
        self, file_path: str, line: int, character: int,
    ) -> list[ReferenceInfo]:
        character = self._resolve_column(file_path, line, character)
        abs_path = self._resolve_path(file_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{abs_path}')\n"
            f"refs = s.get_references(line={line}, column={character})\n"
            f"print(json.dumps([{{'path': str(r.module_path or ''), "
            f"'line': r.line or 0, 'col': r.column or 0}} for r in refs]))"
        )
        output = self._run_python_script(script)
        raw = self._decode_json(output)
        if not isinstance(raw, list):
            return []
        return [
            ReferenceInfo(
                file_path=str(item.get("path", "")),
                line=int(item.get("line", 0) or 0),
                character=int(item.get("col", 0) or 0),
            )
            for item in raw
            if isinstance(item, dict)
        ]

    def _python_hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        character = self._resolve_column(file_path, line, character)
        abs_path = self._resolve_path(file_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path='{abs_path}')\n"
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
        raw = self._decode_json(output)
        if not isinstance(raw, dict):
            return None
        return HoverResult(
            content=str(raw.get("docstring", "")),
            language="python",
        )

    def _python_diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Check Python syntax."""
        try:
            content = Path(file_path).read_text(encoding="utf-8")
            compile(content, file_path, "exec")
            return []
        except FileNotFoundError:
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

    @staticmethod
    def _resolve(result: Any) -> Any:
        """If *result* is a coroutine (async sandbox), run it synchronously."""
        import asyncio
        import inspect

        if inspect.isawaitable(result):
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                loop = None
            if loop and loop.is_running():
                import concurrent.futures
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    return pool.submit(asyncio.run, result).result()
            return asyncio.run(result)
        return result

    def _run_python_script(self, script: str) -> str:
        """Run a Python script locally or in the sandbox.

        For sandbox execution, the script is written to a temp file first
        because ``python3 -c {script!r}`` breaks multi-line scripts: repr
        escapes newlines to literal ``\\n`` which the shell passes as-is,
        causing Python to see a single line with literal backslash-n.
        """
        try:
            if self._sandbox:
                import hashlib
                tag = hashlib.md5(script.encode()).hexdigest()[:8]
                remote_path = f"/tmp/_lsp_query_{tag}.py"
                try:
                    self._sandbox.fs.upload_file(
                        script.encode("utf-8"), remote_path,
                    )
                except Exception:
                    # Fallback: write via shell
                    import shlex
                    self._resolve(self._sandbox.process.exec(
                        f"printf %s {shlex.quote(script)} > {remote_path}",
                        timeout=5,
                    ))
                cmd = f"python3 {remote_path}"
                response = self._resolve(
                    self._sandbox.process.exec(
                        cmd,
                        timeout=int(LSP_QUERY_TIMEOUT),
                    )
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
        return self._check_backend(
            local_cmd=["python3", "-c", "import jedi"],
            sandbox_cmd="python3 -c 'import jedi'",
        )

    def _check_typescript_backend(self) -> bool:
        return self._check_backend(
            local_cmd=["npx", "tsc", "--version"],
            sandbox_cmd="npx tsc --version",
        )

    def _check_backend(self, *, local_cmd: list[str], sandbox_cmd: str) -> bool:
        try:
            if self._sandbox:
                resp = self._resolve(self._sandbox.process.exec(sandbox_cmd, timeout=10))
                return getattr(resp, "exit_code", 1) == 0
            proc = subprocess.run(
                local_cmd,
                capture_output=True, timeout=10,
            )
            return proc.returncode == 0
        except Exception:
            return False

    def _install_python_backend(self) -> bool:
        if not self._sandbox:
            return False
        return self._run_sandbox_install(
            "pip install --quiet --no-cache-dir jedi",
        )

    def _install_typescript_backend(self) -> bool:
        if not self._sandbox:
            return False
        try:
            check = self._resolve(
                self._sandbox.process.exec(
                    "node -e \"require('typescript')\" 2>/dev/null && echo ok || echo missing",
                    timeout=15,
                )
            )
            result = str(getattr(check, "result", "") or "")
            if "missing" not in result:
                return True
        except Exception:
            logger.debug("LSP TypeScript preinstall check failed", exc_info=True)
        return self._run_sandbox_install(
            "npm install --global --quiet typescript",
        )

    def _run_sandbox_install(self, command: str) -> bool:
        try:
            resp = self._resolve(self._sandbox.process.exec(command, timeout=120))
            return getattr(resp, "exit_code", 1) in (0, None)
        except Exception:
            logger.debug("LSP backend install failed: %s", command, exc_info=True)
            return False

    # -- Cache ----------------------------------------------------------------

    def _run_cached_query(
        self,
        key: str,
        loader,
        *,
        cache_when=None,
    ):
        cached = self._get_cached(key)
        if cached is not None:
            return cached

        result = loader()
        should_cache = True if cache_when is None else cache_when(result)
        if should_cache:
            self._put_cached(key, result)
        return result

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
        return _LANGUAGE_BY_EXTENSION.get(ext, "unknown")

    @staticmethod
    def _decode_json(payload: str) -> Any:
        if not payload:
            return None
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            return None


def _coerce_symbol_kind(raw_kind: Any) -> SymbolKind:
    """Map backend-reported symbol types onto SymbolKind."""
    try:
        return SymbolKind(str(raw_kind))
    except ValueError:
        return SymbolKind.UNKNOWN
