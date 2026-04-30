"""Shared query parsing and matching helpers for code intelligence tools."""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sandbox.code_intelligence.core.constants import SKIP_DIRECTORIES, SUPPORTED_EXTENSIONS

logger = logging.getLogger(__name__)
_IDENTIFIER_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_PY_DEF_RE = re.compile(r"^\s*(?:async\s+def|def)\s+([A-Za-z_][A-Za-z0-9_]*)\b")
_PY_CLASS_RE = re.compile(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)\b")
_ASSIGN_RE = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=")


@dataclass(frozen=True)
class _FallbackSearchSpec:
    pattern: str
    kind: str


def _build_fallback_specs(query: str, *, kind: str = "") -> list[_FallbackSearchSpec]:
    """Return ordered regex specs for definition-first fallback lookup."""
    needle = query.strip()
    if not needle:
        return []

    kind_name = (kind or "").lower().strip()
    exact_identifier = bool(_IDENTIFIER_RE.fullmatch(needle))
    escaped = re.escape(needle)
    specs: list[_FallbackSearchSpec] = []

    allow_functions = kind_name in {"", "function", "method"}
    allow_classes = kind_name in {"", "class"}
    allow_vars = kind_name in {"", "variable", "constant", "property"}

    if exact_identifier and allow_functions:
        specs.extend(
            (
                _FallbackSearchSpec(
                    pattern=rf"^\s*(?:async\s+def|def)\s+{escaped}\b",
                    kind="function",
                ),
                _FallbackSearchSpec(
                    pattern=rf"^\s*(?:async\s+def|def)\s+[A-Za-z_][A-Za-z0-9_]*{escaped}[A-Za-z0-9_]*\b",
                    kind="function",
                ),
            )
        )
    if exact_identifier and allow_classes:
        specs.extend(
            (
                _FallbackSearchSpec(
                    pattern=rf"^\s*class\s+{escaped}\b",
                    kind="class",
                ),
                _FallbackSearchSpec(
                    pattern=rf"^\s*class\s+[A-Za-z_][A-Za-z0-9_]*{escaped}[A-Za-z0-9_]*\b",
                    kind="class",
                ),
            )
        )
    if exact_identifier and allow_vars:
        specs.append(
            _FallbackSearchSpec(
                pattern=rf"^\s*{escaped}\s*=",
                kind="variable",
            )
        )

    boundary = rf"\b{escaped}\b" if exact_identifier else escaped
    specs.append(_FallbackSearchSpec(pattern=boundary, kind="text_match"))
    return specs


def _extract_match_name(snippet: str, *, query: str, kind: str) -> str:
    """Infer the real symbol name from a matched line when possible."""
    if kind == "function":
        match = _PY_DEF_RE.search(snippet)
        if match:
            return match.group(1)
    elif kind == "class":
        match = _PY_CLASS_RE.search(snippet)
        if match:
            return match.group(1)
    elif kind == "variable":
        match = _ASSIGN_RE.search(snippet)
        if match:
            return match.group(1)
    return query


def _dedupe_matches(matches: list[dict[str, Any]]) -> list[dict[str, Any]]:
    def _rank(match: dict[str, Any]) -> tuple[int, int, int, str, int]:
        file_path = str(match.get("file") or "")
        lowered = file_path.lower()
        suffix = Path(file_path).suffix.lower()
        kind = str(match.get("kind") or "")
        is_text_match = 1 if kind == "text_match" else 0
        is_doc_path = (
            1
            if (
                suffix in {".md", ".rst", ".txt"}
                or "/docs/" in lowered
                or lowered.endswith("/history.md")
                or lowered.endswith("/readme.md")
            )
            else 0
        )
        depth = file_path.count("/")
        return (is_text_match, is_doc_path, depth, file_path, int(match.get("line") or 0))

    seen: set[tuple[str, int, str, str]] = set()
    deduped: list[dict[str, Any]] = []
    for match in sorted(matches, key=_rank):
        key = (
            str(match.get("file") or ""),
            int(match.get("line") or 0),
            str(match.get("kind") or ""),
            str(match.get("name") or ""),
        )
        if key in seen:
            continue
        seen.add(key)
        deduped.append(match)
    return deduped


def _parse_rg_matches(
    output: str,
    *,
    query: str,
    kind: str,
) -> list[dict[str, Any]]:
    matches: list[dict[str, Any]] = []
    for line in output.splitlines():
        parts = line.split(":", 2)
        if len(parts) != 3:
            continue
        file_path, line_no, snippet = parts
        try:
            parsed_line = int(line_no)
        except ValueError:
            parsed_line = 0
        inferred_name = _extract_match_name(snippet, query=query, kind=kind)
        matches.append(
            {
                "name": inferred_name,
                "kind": kind,
                "file": file_path,
                "line": parsed_line,
                "signature": snippet.strip(),
            }
        )
    return matches


def _python_fallback_query_symbols(
    *,
    root: Path,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    """Last-resort fallback when ripgrep is unavailable."""
    collected: list[dict[str, Any]] = []
    compiled_specs = [
        (re.compile(spec.pattern), spec.kind) for spec in _build_fallback_specs(query, kind=kind)
    ]
    if not compiled_specs:
        return None

    for file_path in root.rglob("*"):
        if not file_path.is_file():
            continue
        if any(part in SKIP_DIRECTORIES for part in file_path.parts):
            continue
        if file_path.suffix.lower() not in SUPPORTED_EXTENSIONS:
            continue
        try:
            lines = file_path.read_text(encoding="utf-8").splitlines()
        except Exception:
            continue
        for lineno, line in enumerate(lines, start=1):
            for pattern, matched_kind in compiled_specs:
                if not pattern.search(line):
                    continue
                collected.append(
                    {
                        "name": _extract_match_name(line, query=query, kind=matched_kind),
                        "kind": matched_kind,
                        "file": str(file_path),
                        "line": lineno,
                        "signature": line.strip(),
                    }
                )
                break

    deduped = _dedupe_matches(collected)
    return deduped or None
