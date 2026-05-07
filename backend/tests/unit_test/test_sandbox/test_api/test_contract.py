"""Contract tests for the public ``sandbox.api`` surface."""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

from sandbox import api as sandbox_api
from sandbox.api import (
    ConflictInfo,
    EditFileResult,
    RawExecResult,
    ReadFileResult,
    SandboxCaller,
    ShellResult,
    WriteFileResult,
)

_API_ROOT = Path(sandbox_api.__file__).parent
_EXPECTED_API_ROOT_ENTRIES = {
    "__init__.py",
    "facade.py",
    "status",
    "tool",
    "utils",
}
_MODEL_ONLY_MODULES = {
    "__init__.py",
    "tool/__init__.py",
}
_PUBLIC_VERB_IMPORT_ALLOWLIST = {
    "tool/read.py": {"sandbox.api.tool._runtime"},
    "tool/write.py": {
        "sandbox.api.tool._runtime",
    },
    "tool/edit.py": {
        "sandbox.api.tool._runtime",
    },
    "tool/shell.py": {
        "sandbox.api.tool._runtime",
    },
    "status/__init__.py": {
        "sandbox.control.ops.recovery",
        "sandbox.control.ops.setup",
        "sandbox.providers.registry",
    },
}
_FORBIDDEN_FOR_MODELS = (
    "sandbox.providers",
    "sandbox.daytona",
    "sandbox.runtime",
    "sandbox.occ",
    "sandbox.overlay",
    "tools.",
    "daytona_sdk",
)


def _imported_modules(source: str) -> set[str]:
    tree = ast.parse(source)
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names


def test_api_root_keeps_public_surface_grouped_by_role() -> None:
    assert {
        path.name
        for path in _API_ROOT.iterdir()
        if path.name != "__pycache__" and not path.name.startswith(".")
    } == _EXPECTED_API_ROOT_ENTRIES


@pytest.mark.parametrize(
    "module_path",
    sorted(
        [
            *_API_ROOT.glob("*.py"),
            *(_API_ROOT / "tool").glob("*.py"),
            _API_ROOT / "status" / "__init__.py",
        ]
    ),
)
def test_api_import_boundaries(module_path: Path) -> None:
    module_id = module_path.relative_to(_API_ROOT).as_posix()
    source = module_path.read_text(encoding="utf-8")
    imported = _imported_modules(source)
    if module_id in _MODEL_ONLY_MODULES:
        for name in imported:
            for forbidden in _FORBIDDEN_FOR_MODELS:
                assert not (
                    name == forbidden.rstrip(".") or name.startswith(forbidden)
                ), f"{module_id} imports forbidden module {name!r}"
        return

    allowed = _PUBLIC_VERB_IMPORT_ALLOWLIST.get(module_id)
    if allowed is None:
        return
    for name in imported:
        if name.startswith("sandbox.api"):
            continue
        assert name in allowed or name.split(".")[0] not in {"sandbox", "tools"}, (
            f"{module_id} imports non-public dependency {name!r}"
        )


def test_sandbox_caller_defaults_and_immutability() -> None:
    caller = SandboxCaller(agent_id="worker-1")
    assert caller.agent_id == "worker-1"
    assert caller.run_id == ""
    assert caller.agent_run_id == ""
    assert caller.task_id == ""
    with pytest.raises((AttributeError, TypeError)):
        caller.agent_id = "b"  # type: ignore[misc]


def test_result_hierarchy_exposes_conflict_only_on_guarded_results() -> None:
    assert not hasattr(ReadFileResult(content="x"), "conflict")
    assert not hasattr(RawExecResult(exit_code=0, stdout="x"), "conflict")

    conflict = ConflictInfo(reason="base_mismatch", conflict_file="/repo/a.py")
    for result in (
        WriteFileResult(success=False, conflict=conflict, conflict_reason=conflict.reason),
        EditFileResult(success=False, conflict=conflict, conflict_reason=conflict.reason),
        ShellResult(
            success=False,
            exit_code=0,
            stdout="",
            conflict=conflict,
            conflict_reason=conflict.reason,
        ),
    ):
        assert result.conflict is conflict
        assert result.conflict_reason == "base_mismatch"


def test_sandbox_toolkit_keeps_shared_mutation_tool_result() -> None:
    from tools.sandbox_toolkit.mutation_result import mutation_tool_result

    assert callable(mutation_tool_result)
