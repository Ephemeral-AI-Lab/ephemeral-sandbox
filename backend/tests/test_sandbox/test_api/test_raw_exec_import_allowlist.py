"""Import allowlist for un-guarded raw sandbox execution."""

from __future__ import annotations

import ast
from pathlib import Path


SRC_ROOT = Path(__file__).resolve().parents[3] / "src"

_DEBUG_IMPORTERS = {
    Path("server/routers/sandboxes.py"),
}


def test_raw_exec_imports_are_allowlisted() -> None:
    offenders: list[str] = []
    for module in _python_files(SRC_ROOT):
        if module.relative_to(SRC_ROOT) == Path("sandbox/api/raw_exec.py"):
            continue
        if not _imports_raw_exec(module):
            continue
        rel = module.relative_to(SRC_ROOT)
        if not _is_allowlisted(rel):
            offenders.append(rel.as_posix())

    assert offenders == []


def _is_allowlisted(path: Path) -> bool:
    return (
        path == Path("sandbox/api/__init__.py")
        or path == Path("sandbox/api/read.py")
        or path == Path("sandbox/api/shell.py")
        or path == Path("sandbox/runtime/bundle.py")
        or path == Path("sandbox/runtime/setup_orchestrator.py")
        or path in {
            Path("sandbox/providers/daytona/lifecycle.py"),
            Path("sandbox/providers/daytona/proxy.py"),
        }
        or path in _DEBUG_IMPORTERS
    )


def _python_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.py") if "__pycache__" not in path.parts)


def _imports_raw_exec(path: Path) -> bool:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            if any(alias.name == "sandbox.api.raw_exec" for alias in node.names):
                return True
        elif isinstance(node, ast.ImportFrom):
            if node.module == "sandbox.api.raw_exec":
                return True
            if node.module == "sandbox.api" and any(
                alias.name == "raw_exec" for alias in node.names
            ):
                return True
    return False
