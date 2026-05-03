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
    RequestActor,
    ShellResult,
    WriteFileResult,
)

_API_ROOT = Path(sandbox_api.__file__).parent
_MODEL_ONLY_MODULES = {
    "__init__.py",
    "models.py",
}
_PUBLIC_VERB_IMPORT_ALLOWLIST = {
    "read.py": {"sandbox.api.raw_exec"},
    "write.py": {"sandbox.occ.client", "sandbox.occ.types"},
    "edit.py": {
        "sandbox.occ.client",
        "sandbox.occ.patching.patcher",
        "sandbox.occ.types",
    },
    "shell.py": {"sandbox.overlay.client", "sandbox.overlay.types"},
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


@pytest.mark.parametrize("module_path", sorted(_API_ROOT.glob("*.py")))
def test_api_import_boundaries(module_path: Path) -> None:
    source = module_path.read_text(encoding="utf-8")
    imported = _imported_modules(source)
    if module_path.name in _MODEL_ONLY_MODULES:
        for name in imported:
            for forbidden in _FORBIDDEN_FOR_MODELS:
                assert not (
                    name == forbidden.rstrip(".") or name.startswith(forbidden)
                ), f"{module_path.name} imports forbidden module {name!r}"
        return

    allowed = _PUBLIC_VERB_IMPORT_ALLOWLIST.get(module_path.name)
    if allowed is None:
        return
    for name in imported:
        if name.startswith("sandbox.api"):
            continue
        assert name in allowed or name.split(".")[0] not in {"sandbox", "tools"}, (
            f"{module_path.name} imports non-public dependency {name!r}"
        )


def test_request_actor_defaults_and_immutability() -> None:
    actor = RequestActor(agent_id="worker-1")
    assert actor.agent_id == "worker-1"
    assert actor.run_id == ""
    assert actor.agent_run_id == ""
    assert actor.task_id == ""
    with pytest.raises((AttributeError, TypeError)):
        actor.agent_id = "b"  # type: ignore[misc]


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


def test_legacy_api_modules_are_deleted() -> None:
    import importlib.util

    for module_name in (
        "sandbox.api.audited_sandbox_api",
        "sandbox.api.sandbox_api",
        "sandbox.api.audit",
        "sandbox.api.attribution",
        "sandbox.api.transport",
        "sandbox.daytona.transport",
        "tools.sandbox_toolkit._mutation_result",
        "tools.core.op_result_to_tool_result",
    ):
        assert importlib.util.find_spec(module_name) is None
