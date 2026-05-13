"""Import-fence tests for shared audit primitives."""

from __future__ import annotations

import ast
from pathlib import Path


SRC_ROOT = Path(__file__).resolve().parents[3] / "src"
AUDIT_ROOT = SRC_ROOT / "audit"


def test_audit_base_has_no_domain_dependencies() -> None:
    imports = _imports(AUDIT_ROOT / "base.py")
    forbidden_prefixes = ("task_center", "engine", "sandbox", "live_e2e")

    offenders = [
        imported
        for imported in imports
        if any(
            imported == prefix or imported.startswith(f"{prefix}.")
            for prefix in forbidden_prefixes
        )
    ]

    assert offenders == []


def test_audit_bus_depends_only_on_base_and_standard_library() -> None:
    allowed = {
        "__future__",
        "collections.abc",
        "dataclasses",
        "audit.base",
    }

    assert _imports(AUDIT_ROOT / "bus.py") <= allowed


def _imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names
