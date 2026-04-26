"""Python/Jedi backend implementation for the LSP facade."""

from __future__ import annotations

import json
import logging
import re
import shlex
import threading
from collections import OrderedDict
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING, Any

from code_intelligence.core.async_bridge import run_sync
from code_intelligence.core.path_utils import resolve_workspace_path
from code_intelligence.core.types import (
    Diagnostic,
    DiagnosticSeverity,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)
from code_intelligence.language_server.utils import _coerce_symbol_kind

logger = logging.getLogger("code_intelligence.language_server.client")


class PythonBackendMixin:
    _workspace_root: str
    _sandbox: Any
    _cache_max: int
    _line_cache_lock: threading.Lock
    _line_cache: OrderedDict[tuple[str, int], str | None]

    if TYPE_CHECKING:
        def _run_python_script(self, script: str) -> str: ...

    def _resolve_path(self, file_path: str) -> str:
        """Resolve a potentially relative file path against workspace root."""
        return resolve_workspace_path(file_path, self._workspace_root)

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

    def _python_definitions(
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> list[SymbolInfo]:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

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
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> list[ReferenceInfo]:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

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

    def _python_references_many(
        self,
        requests: Sequence[tuple[str, int, int]],
    ) -> list[list[ReferenceInfo]]:
        payload = [
            {"path": path, "line": int(line), "column": int(character)}
            for path, line, character in requests
        ]
        payload_literal = json.dumps(payload)
        script = (
            "import concurrent.futures, jedi, json\n"
            f"requests = json.loads({payload_literal!r})\n"
            "def one(req):\n"
            "    try:\n"
            "        s = jedi.Script(path=req['path'])\n"
            "        refs = s.get_references(\n"
            "            line=int(req['line']), column=int(req['column'])\n"
            "        )\n"
            "        return [\n"
            "            {'path': str(r.module_path or ''), 'line': r.line or 0, 'col': r.column or 0}\n"
            "            for r in refs\n"
            "        ]\n"
            "    except Exception as exc:\n"
            "        return {'__error__': str(exc)}\n"
            "workers = min(32, max(1, len(requests)))\n"
            "with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:\n"
            "    results = list(pool.map(one, requests))\n"
            "print(json.dumps(results))\n"
        )
        output = self._run_python_script(script)
        raw = self._decode_json(output)
        if not isinstance(raw, list):
            return [[] for _ in requests]
        results: list[list[ReferenceInfo]] = []
        for item in raw[: len(requests)]:
            if not isinstance(item, list):
                if isinstance(item, dict) and "__error__" in item:
                    logger.debug("jedi batch references failed: %s", item.get("__error__"))
                results.append([])
                continue
            results.append(
                [
                    ReferenceInfo(
                        file_path=str(ref.get("path", "")),
                        line=int(ref.get("line", 0) or 0),
                        character=int(ref.get("col", 0) or 0),
                    )
                    for ref in item
                    if isinstance(ref, dict)
                ]
            )
        while len(results) < len(requests):
            results.append([])
        return results

    def _python_hover(
        self,
        file_path: str,
        line: int,
        character: int,
    ) -> HoverResult | None:
        character = self._resolve_column(file_path, line, character)
        resolved_path = self._resolve_path(file_path)

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
        resolved_path = self._resolve_path(file_path)
        if self._sandbox:
            path_literal = json.dumps(resolved_path)
            script = (
                "import json\n"
                "from pathlib import Path\n"
                f"path = {path_literal}\n"
                "try:\n"
                "    content = Path(path).read_text(encoding='utf-8')\n"
                "    compile(content, path, 'exec')\n"
                "except FileNotFoundError:\n"
                "    print(json.dumps({'type': 'missing'}))\n"
                "except SyntaxError as exc:\n"
                "    print(json.dumps({\n"
                "        'type': 'syntax_error',\n"
                "        'line': exc.lineno or 0,\n"
                "        'character': (exc.offset or 1) - 1,\n"
                "        'message': str(exc.msg),\n"
                "    }))\n"
                "else:\n"
                "    print(json.dumps({'type': 'clean'}))\n"
            )
            output = self._run_python_script(script)
            raw = self._decode_json(output)
            if not isinstance(raw, dict):
                raise RuntimeError("LSP diagnostics unavailable: python query produced no JSON")
            result_type = raw.get("type")
            if result_type in {"clean", "missing"}:
                return []
            if result_type != "syntax_error":
                raise RuntimeError(
                    f"LSP diagnostics unavailable: unexpected result {result_type!r}"
                )
            return [
                Diagnostic(
                    file_path=file_path,
                    line=int(raw.get("line", 0) or 0),
                    character=int(raw.get("character", 0) or 0),
                    severity=DiagnosticSeverity.ERROR,
                    message=str(raw.get("message", "")),
                    source="python",
                )
            ]

        try:
            content = Path(resolved_path).read_text(encoding="utf-8")
            compile(content, resolved_path, "exec")
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

    def _detect_language(self, file_path: str) -> str:
        ext = Path(file_path).suffix.lower()
        return "python" if ext == ".py" else "unknown"

    @staticmethod
    def _decode_json(payload: str) -> Any:
        if not payload:
            return None
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            return None
