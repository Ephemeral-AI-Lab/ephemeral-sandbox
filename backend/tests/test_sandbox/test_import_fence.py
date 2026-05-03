"""Import-fence tests for the sandbox API adapter migration."""

from __future__ import annotations

import ast
import importlib.util
from pathlib import Path


SRC_ROOT = Path(__file__).resolve().parents[2] / "src"


def test_no_forbidden_daytona_imports() -> None:
    forbidden_for_tools = (
        "sandbox.daytona",
        "sandbox.runtime",
        "tools.core.ci_adapter",
        "tools.core.sandbox_commit",
        "tools.core.ci_attribution",
    )
    forbidden_for_ci_internals = ("sandbox.daytona", "daytona_sdk")

    for module in _python_files(SRC_ROOT / "tools" / "sandbox_toolkit"):
        _assert_no_imports(module, forbidden_for_tools)
    for module in _python_files(SRC_ROOT / "sandbox" / "code_intelligence"):
        _assert_no_imports(module, forbidden_for_ci_internals)


def test_sandbox_code_intelligence_package_is_deleted() -> None:
    assert importlib.util.find_spec("sandbox.code_intelligence") is None


def _python_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.py") if "__pycache__" not in path.parts)


def _assert_no_imports(path: Path, forbidden: tuple[str, ...]) -> None:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    for node in ast.walk(tree):
        for imported in _imported_modules(node):
            for prefix in forbidden:
                assert imported != prefix and not imported.startswith(f"{prefix}."), (
                    f"{path.relative_to(SRC_ROOT)} imports forbidden module {imported!r}"
                )


def _imported_modules(node: ast.AST) -> tuple[str, ...]:
    if isinstance(node, ast.Import):
        return tuple(alias.name for alias in node.names)
    if isinstance(node, ast.ImportFrom):
        return (node.module,) if node.module else ()
    return ()
