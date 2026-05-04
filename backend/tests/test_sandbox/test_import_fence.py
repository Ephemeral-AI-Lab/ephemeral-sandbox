"""Import-fence tests for the sandbox public API cutover."""

from __future__ import annotations

import ast
import importlib
import importlib.util
from pathlib import Path

import pytest


SRC_ROOT = Path(__file__).resolve().parents[2] / "src"
_TOOL_ALLOWED = {
    "sandbox.api",
    "sandbox.api.edit",
    "sandbox.api.read",
    "sandbox.api.shell",
    "sandbox.api.write",
}
_TOOL_FORBIDDEN_PREFIXES = (
    "sandbox.api.raw_exec",
    "sandbox.providers",
    "sandbox.occ",
    "sandbox.overlay",
    "sandbox.runtime",
    "sandbox.daytona",
    "sandbox.code_intelligence",
)


def test_agent_sandbox_tools_import_only_public_api_verbs() -> None:
    offenders: list[str] = []
    for module in _python_files(SRC_ROOT / "tools" / "sandbox_toolkit"):
        for imported in _imports(module):
            if not imported.startswith("sandbox."):
                continue
            if imported in _TOOL_ALLOWED:
                continue
            if any(
                imported == prefix or imported.startswith(f"{prefix}.")
                for prefix in _TOOL_FORBIDDEN_PREFIXES
            ):
                offenders.append(
                    f"{module.relative_to(SRC_ROOT)} imports {imported}"
                )
                continue
            offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_deleted_legacy_sandbox_modules_are_unimportable() -> None:
    for module_name in (
        "sandbox.code_intelligence",
        "sandbox.api._changeset_projection",
        "sandbox.api.bash",
        "sandbox.api.models",
        "sandbox.api.shell_routing",
        "sandbox.api.file_commands",
        "sandbox.api.transport",
        "sandbox.api.audited_sandbox_api",
        "sandbox.daytona.transport",
    ):
        assert importlib.util.find_spec(module_name) is None


def test_deleted_code_intelligence_package_raises_module_not_found() -> None:
    with pytest.raises(ModuleNotFoundError):
        importlib.import_module("sandbox.code_intelligence")


def test_deleted_sandbox_transport_symbol_raises_import_error() -> None:
    with pytest.raises(ImportError):
        __import__("sandbox.api.transport", fromlist=["SandboxTransport"])


def _python_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.py") if "__pycache__" not in path.parts)


def _imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names
