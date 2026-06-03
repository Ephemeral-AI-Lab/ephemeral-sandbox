"""Boundary tests for sandbox API usage by agent runtime and tools."""

from __future__ import annotations

import ast
from pathlib import Path


SRC_ROOT = Path(__file__).resolve().parents[2] / "src"
SCAN_ROOTS = (
    SRC_ROOT / "engine" / "runtime",
    SRC_ROOT / "tools",
)
FORBIDDEN_PREFIXES = (
    "sandbox.host",
    "sandbox.lifecycle",
    "sandbox.provider",
    "sandbox.daemon",
)
ALLOWED_API_IMPORT_NAMES = {
    "ConflictInfo",
    "EditFileRequest",
    "EditFileResult",
    "GuardedResultBase",
    "RawExecResult",
    "ReadFileRequest",
    "ReadFileResult",
    "SandboxCaller",
    "SandboxResultBase",
    "SearchReplaceEdit",
    "WriteFileRequest",
    "WriteFileResult",
}

def test_daemon_and_tools_reach_sandbox_through_api_facade() -> None:
    offenders: list[str] = []
    for path in _python_files(SCAN_ROOTS):
        source = path.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    imported = alias.name
                    if _is_forbidden_import(imported):
                        offenders.append(_format(path, source, node))
                    elif imported.startswith("sandbox.api."):
                        offenders.append(_format(path, source, node))
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported = node.module
                if _is_forbidden_import(imported):
                    offenders.append(_format(path, source, node))
                elif imported == "sandbox.api":
                    bad_names = [
                        alias.name
                        for alias in node.names
                        if alias.name not in ALLOWED_API_IMPORT_NAMES
                    ]
                    if bad_names:
                        offenders.append(_format(path, source, node))
                elif imported.startswith("sandbox.api."):
                    offenders.append(_format(path, source, node))

    assert offenders == []


def _python_files(roots: tuple[Path, ...]) -> list[Path]:
    files: list[Path] = []
    for root in roots:
        files.extend(
            path for path in root.rglob("*.py") if "__pycache__" not in path.parts
        )
    return sorted(files)


def _is_forbidden_import(imported: str) -> bool:
    return any(
        imported == prefix or imported.startswith(f"{prefix}.")
        for prefix in FORBIDDEN_PREFIXES
    )


def _format(path: Path, source: str, node: ast.AST) -> str:
    statement = ast.get_source_segment(source, node) or "<unknown import>"
    rel = path.relative_to(SRC_ROOT)
    return f"{rel}:{getattr(node, 'lineno', 0)} {statement}"
