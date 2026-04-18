"""Semantic language-server-backed code intelligence queries."""

from __future__ import annotations

import base64
import json
import logging
import re
import shlex
import subprocess
import threading
import time
from collections import OrderedDict
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)

from code_intelligence._async_bridge import run_sync
from code_intelligence.constants import (
    LSP_CACHE_MAX_ENTRIES,
    LSP_CACHE_TTL,
    LSP_QUERY_TIMEOUT,
)
from code_intelligence.lsp._jedi_worker_client import (
    BaseJediWorkerClient,
    JediWorkerClient,
    SandboxJediWorkerClient,
    WorkerUnavailable,
    is_enabled as jedi_worker_enabled,
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
class _InflightQuery:
    """One in-progress cached query shared by concurrent callers."""

    event: threading.Event
    result: Any = None
    error: BaseException | None = None


@dataclass
class LspTelemetry:
    """LSP client telemetry."""

    queries: int = 0
    errors: int = 0
    successes: int = 0
    cache_hits: int = 0
    script_runs: int = 0
    script_successes: int = 0
    script_errors: int = 0
    worker_successes: int = 0
    worker_fallbacks: int = 0
    worker_errors: int = 0


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
        self._line_cache_lock = threading.Lock()

        self._cache: OrderedDict[str, _CacheEntry] = OrderedDict()
        self._inflight: dict[str, _InflightQuery] = {}
        self._line_cache: OrderedDict[tuple[str, int], str | None] = OrderedDict()
        self._telemetry = LspTelemetry()
        self._py_available: bool | None = None
        self._ts_available: bool | None = None
        self._worker_enabled_default = sandbox is not None

        # Persistent Jedi worker (local stdio or sandbox socket, env-gated).
        # Built lazily on first successful use; torn down on client close.
        self._worker: BaseJediWorkerClient | None = None
        self._worker_lock = threading.Lock()

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

    def rename_symbol(
        self, file_path: str, line: int, character: int, new_name: str,
    ) -> dict[str, str]:
        """Return {absolute_path: new_content} for every file affected by the rename.

        Empty dict means the symbol could not be resolved or produced no changes.
        Renames are computed but never written — callers coordinate the writes.
        """
        language = self._detect_language(file_path)
        if language != "python":
            return {}
        return self._python_rename(file_path, line, character, new_name)

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
        """Invalidate all cached results for a file.

        Also forwards the invalidation to the persistent Jedi worker
        (when enabled) so Jedi's internal module cache drops this path.
        Worker failures are swallowed — a stale worker cache is a
        performance concern, not a correctness concern; the next query
        will miss the cache and re-resolve.
        """
        with self._cache_lock:
            to_remove = [k for k in self._cache if file_path in k]
            for k in to_remove:
                del self._cache[k]
        resolved_path = self._resolve_path(file_path)
        with self._line_cache_lock:
            stale = [key for key in self._line_cache if key[0] == resolved_path]
            for key in stale:
                del self._line_cache[key]
        client = self._existing_worker()
        if client is None:
            return
        try:
            client.request("invalidate", {"path": self._resolve_path(file_path)})
        except WorkerUnavailable:
            pass
        except Exception:  # pragma: no cover - defensive
            logger.debug("worker invalidate failed for %s", file_path, exc_info=True)

    def close(self) -> None:
        """Shut down the persistent Jedi worker, if one is running."""
        with self._worker_lock:
            worker = self._worker
            self._worker = None
        if worker is not None:
            worker.shutdown()

    def ensure_ready(
        self,
        *,
        install_missing: bool = False,
        languages: Sequence[str] | None = None,
    ) -> dict[str, bool]:
        """Check which language backends are available.

        When attached to a sandbox, optionally install bounded missing
        dependencies so CI can recover from a cold image. ``languages``
        scopes the probe so Python-only CI flows do not pay for unrelated
        TypeScript process startup.
        """
        targets = _readiness_targets(languages)
        if "python" in targets and self._py_available is None:
            self._py_available = self._check_python_backend()
        if "typescript" in targets and self._ts_available is None:
            self._ts_available = self._check_typescript_backend()
        if install_missing and self._sandbox:
            if "python" in targets and not self._py_available:
                self._py_available = self._install_python_backend()
            if "typescript" in targets and not self._ts_available:
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
                script_runs=self._telemetry.script_runs,
                script_successes=self._telemetry.script_successes,
                script_errors=self._telemetry.script_errors,
                worker_successes=self._telemetry.worker_successes,
                worker_fallbacks=self._telemetry.worker_fallbacks,
                worker_errors=self._telemetry.worker_errors,
            )

    @property
    def connected(self) -> bool:
        """Whether at least one language backend is available."""
        status = self.ensure_ready(languages=("python",))
        return bool(status.get("python")) or bool(self._ts_available)

    @property
    def worker_active(self) -> bool:
        """Whether a persistent Jedi worker has been created for this client."""
        with self._worker_lock:
            return self._worker is not None

    def worker_status(self) -> dict[str, Any]:
        """Return worker metadata without creating a worker."""
        with self._worker_lock:
            worker = self._worker
        status: dict[str, Any] = {
            "enabled": self._worker_enabled(),
            "active": worker is not None,
        }
        if worker is None:
            return status
        try:
            status.update(worker.worker_status())
        except Exception:  # pragma: no cover - defensive observability path
            logger.debug("failed to read jedi worker status", exc_info=True)
        return status

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

    _DEF_CLASS_RE = re.compile(r"^(\s*(?:async\s+)?(?:def|class)\s+)")
    def _resolve_column(self, file_path: str, line: int, character: int) -> int:
        """When character is 0, advance to the actual symbol name column.

        Jedi's ``help()``, ``get_references()``, and ``goto()`` need the
        cursor on the actual symbol text.  Callers (ci_query_symbol)
        often pass ``character=0`` which lands on leading indentation —
        producing empty results.

        For ``def``/``class`` lines the cursor is placed on the symbol
        name (after the keyword), not on ``def``/``class`` itself, so
        Jedi resolves the function/class rather than the keyword.

        Returns the resolved column (0-indexed).
        """
        if character != 0:
            return character
        try:
            text = self._read_line(file_path, line)
            if text is None:
                return 0
            stripped = text.lstrip()
            if not stripped:
                return 0
            indent = len(text) - len(stripped)
            # For def/class lines, jump past the keyword to the symbol name
            m = self._DEF_CLASS_RE.match(text)
            if m:
                return len(m.group(1))
            return indent
        except Exception:
            logger.debug("_resolve_column failed for %s:%d", file_path, line)
            return 0

    def _read_line(self, file_path: str, line: int) -> str | None:
        """Read a single line from a local or sandbox file (1-indexed)."""
        abs_path = self._resolve_path(file_path)
        key = (abs_path, int(line))
        with self._line_cache_lock:
            if key in self._line_cache:
                self._line_cache.move_to_end(key)
                return self._line_cache[key]
            value = self._read_line_uncached(abs_path, int(line))
            self._line_cache[key] = value
            self._line_cache.move_to_end(key)
            while len(self._line_cache) > self._cache_max:
                self._line_cache.popitem(last=False)
            return value

    def _read_line_uncached(self, abs_path: str, line: int) -> str | None:
        """Read a single resolved line without consulting the local cache."""
        try:
            if self._sandbox:
                resp = run_sync(
                    self._sandbox.process.exec(
                        f"sed -n {int(line)}p {shlex.quote(abs_path)}",
                        timeout=5,
                    )
                )
                return str(getattr(resp, "result", "") or "")
            p = Path(abs_path)
            if not p.exists():
                return None
            lines = p.read_text(encoding="utf-8").splitlines()
            if line < 1 or line > len(lines):
                return None
            return lines[line - 1]
        except Exception:
            return None

    # -- Persistent worker dispatch ------------------------------------------
    #
    # The worker amortises Jedi's import/project cost across Python
    # queries. Local mode uses stdio; sandbox mode runs a socket daemon
    # inside the sandbox and reaches it through small process.exec RPCs.

    def _get_worker(self) -> BaseJediWorkerClient | None:
        if not self._worker_enabled():
            return None
        with self._worker_lock:
            if self._worker is None:
                if self._sandbox is None:
                    self._worker = JediWorkerClient(
                        self._workspace_root,
                        enabled_default=self._worker_enabled_default,
                    )
                else:
                    self._worker = SandboxJediWorkerClient(
                        self._workspace_root,
                        sandbox=self._sandbox,
                        enabled_default=self._worker_enabled_default,
                    )
            return self._worker

    def _existing_worker(self) -> BaseJediWorkerClient | None:
        """Return the current worker without creating one."""
        with self._worker_lock:
            return self._worker

    def _worker_enabled(self) -> bool:
        return jedi_worker_enabled(default=self._worker_enabled_default)

    def _try_worker(self, op: str, args: dict[str, Any]) -> tuple[bool, Any]:
        """Attempt a worker request. Returns ``(ok, result_or_None)``.

        On ``WorkerUnavailable`` the caller must fall back to the
        subprocess-per-call path. Logical worker errors (``ok=False``
        response) surface as ``(True, None)`` — the worker is healthy
        but the query failed, matching the subprocess path's behaviour
        of returning an empty result for bad positions.
        """
        client = self._get_worker()
        if client is None:
            return False, None
        try:
            result = client.request(op, args)
            self._record_worker_success()
            return True, result
        except WorkerUnavailable:
            self._record_worker_fallback()
            return False, None
        except Exception:  # pragma: no cover - defensive
            self._record_worker_error()
            logger.debug("jedi worker op %s failed", op, exc_info=True)
            return True, None

    def _python_definitions(
        self, file_path: str, line: int, character: int,
    ) -> list[SymbolInfo]:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

        used, result = self._try_worker(
            "definitions",
            {"path": resolved_path, "line": int(line), "column": int(character)},
        )
        if used:
            items = result if isinstance(result, list) else []
            return [
                SymbolInfo(
                    name=str(item.get("name", "")),
                    kind=_coerce_symbol_kind(item.get("type")),
                    file_path=str(item.get("module_path", "")),
                    line=int(item.get("line", 0) or 0),
                    character=int(item.get("column", 0) or 0),
                )
                for item in items
                if isinstance(item, dict) and item.get("name")
            ]

        path_literal = json.dumps(resolved_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path={path_literal})\n"
            f"defs = s.goto(line={line}, column={character}, follow_imports=True)\n"
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
        resolved_path = self._resolve_path(file_path)

        used, result = self._try_worker(
            "references",
            {"path": resolved_path, "line": int(line), "column": int(character)},
        )
        if used:
            items = result if isinstance(result, list) else []
            return [
                ReferenceInfo(
                    file_path=str(item.get("module_path", "")),
                    line=int(item.get("line", 0) or 0),
                    character=int(item.get("column", 0) or 0),
                )
                for item in items
                if isinstance(item, dict)
            ]

        path_literal = json.dumps(resolved_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path={path_literal})\n"
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

    def _python_rename(
        self, file_path: str, line: int, character: int, new_name: str,
    ) -> dict[str, str]:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

        used, result = self._try_worker(
            "rename",
            {
                "path": resolved_path,
                "line": int(line),
                "column": int(character),
                "new_name": str(new_name),
            },
        )
        if used:
            if isinstance(result, dict):
                return {str(k): str(v) for k, v in result.items() if isinstance(v, str)}
            return {}

        path_literal = json.dumps(resolved_path)
        new_name_literal = json.dumps(str(new_name))
        script = (
            "import jedi, json\n"
            f"s = jedi.Script(path={path_literal})\n"
            "out = {}\n"
            "try:\n"
            f"    r = s.rename(line={line}, column={character}, new_name={new_name_literal})\n"
            "    for p, cf in r.get_changed_files().items():\n"
            "        try:\n"
            "            out[str(p)] = cf.get_new_code()\n"
            "        except Exception:\n"
            "            continue\n"
            "except Exception as exc:\n"
            "    print(json.dumps({'__error__': str(exc)}))\n"
            "else:\n"
            "    print(json.dumps(out))\n"
        )
        output = self._run_python_script(script)
        raw = self._decode_json(output)
        if not isinstance(raw, dict):
            return {}
        if "__error__" in raw:
            logger.debug("jedi rename failed: %s", raw.get("__error__"))
            return {}
        return {str(k): str(v) for k, v in raw.items() if isinstance(v, str)}

    def _python_hover(
        self, file_path: str, line: int, character: int,
    ) -> HoverResult | None:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

        used, result = self._try_worker(
            "hover",
            {"path": resolved_path, "line": int(line), "column": int(character)},
        )
        if used:
            if not isinstance(result, dict):
                return None
            return HoverResult(
                content=str(result.get("docstring", "")),
                language="python",
            )

        path_literal = json.dumps(resolved_path)
        script = (
            f"import jedi, json\n"
            f"s = jedi.Script(path={path_literal})\n"
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

    def _run_python_script(self, script: str) -> str:
        """Run a Python script locally or in the sandbox.

        For sandbox execution, base64 transport avoids marker-collision and
        shell-quoting edge cases while keeping the query to one ``process.exec``.
        """
        self._record_script_run()
        try:
            if self._sandbox:
                payload = base64.b64encode(script.encode("utf-8")).decode("ascii")
                cmd = f"echo {shlex.quote(payload)} | base64 -d | python3 -"
                response = run_sync(
                    self._sandbox.process.exec(
                        _wrap_bash_command(cmd),
                        timeout=int(LSP_QUERY_TIMEOUT),
                    )
                )
                result = response.result or ""
                result, exit_code = _extract_exit_code(
                    result,
                    fallback_exit_code=getattr(response, "exit_code", None),
                )
                if exit_code not in (0, None):
                    raise RuntimeError(result or "sandbox python LSP query failed")
            else:
                proc = subprocess.run(
                    [__import__("sys").executable, "-c", script],
                    capture_output=True,
                    text=True,
                    timeout=LSP_QUERY_TIMEOUT,
                    cwd=self._workspace_root or None,
                )
                result = proc.stdout
            self._record_success()
            self._record_script_success()
            return result.strip()
        except Exception as e:
            self._record_error()
            self._record_script_error()
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
                exit_code = self._run_sandbox_command_exit_code(sandbox_cmd, timeout=10)
                return exit_code == 0
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
            "python3 -m pip install --quiet --no-cache-dir jedi",
        )

    def _install_typescript_backend(self) -> bool:
        if not self._sandbox:
            return False
        try:
            check = run_sync(
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
            exit_code = self._run_sandbox_command_exit_code(command, timeout=120)
            return exit_code == 0
        except Exception:
            logger.debug("LSP backend install failed: %s", command, exc_info=True)
            return False

    def _run_sandbox_command_exit_code(self, command: str, *, timeout: int) -> int:
        """Run a sandbox command and recover its shell exit code."""
        response = run_sync(
            self._sandbox.process.exec(
                _wrap_bash_command(command),
                timeout=timeout,
            )
        )
        result = str(getattr(response, "result", "") or "")
        _cleaned, exit_code = _extract_exit_code(
            result,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code

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

        with self._cache_lock:
            inflight = self._inflight.get(key)
            if inflight is None:
                inflight = _InflightQuery(event=threading.Event())
                self._inflight[key] = inflight
                owner = True
            else:
                owner = False

        if not owner:
            inflight.event.wait()
            if inflight.error is not None:
                raise inflight.error
            return inflight.result

        try:
            result = loader()
            should_cache = True if cache_when is None else cache_when(result)
            with self._cache_lock:
                if should_cache:
                    self._cache[key] = _CacheEntry(
                        result=result,
                        expires_at=time.time() + self._cache_ttl,
                    )
                    self._cache.move_to_end(key)
                    while len(self._cache) > self._cache_max:
                        self._cache.popitem(last=False)
                inflight.result = result
            return result
        except BaseException as exc:
            with self._cache_lock:
                inflight.error = exc
            raise
        finally:
            with self._cache_lock:
                self._inflight.pop(key, None)
                inflight.event.set()

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

    def _record_script_run(self) -> None:
        with self._counter_lock:
            self._telemetry.script_runs += 1

    def _record_script_success(self) -> None:
        with self._counter_lock:
            self._telemetry.script_successes += 1

    def _record_script_error(self) -> None:
        with self._counter_lock:
            self._telemetry.script_errors += 1

    def _record_worker_success(self) -> None:
        with self._counter_lock:
            self._telemetry.successes += 1
            self._telemetry.worker_successes += 1

    def _record_worker_fallback(self) -> None:
        with self._counter_lock:
            self._telemetry.worker_fallbacks += 1

    def _record_worker_error(self) -> None:
        with self._counter_lock:
            self._telemetry.errors += 1
            self._telemetry.worker_errors += 1

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


def _readiness_targets(languages: Sequence[str] | None) -> set[str]:
    if languages is None:
        return {"python", "typescript"}
    return {str(language).strip().lower() for language in languages if str(language).strip()}


def _coerce_symbol_kind(raw_kind: Any) -> SymbolKind:
    """Map backend-reported symbol types onto SymbolKind."""
    try:
        return SymbolKind(str(raw_kind))
    except ValueError:
        return SymbolKind.UNKNOWN
