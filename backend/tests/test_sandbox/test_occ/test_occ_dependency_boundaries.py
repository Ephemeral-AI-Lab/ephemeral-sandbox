"""Dependency boundary checks for Phase 03 OCC preparation."""

from __future__ import annotations

import ast
from pathlib import Path

import sandbox.occ
import sandbox.runtime.overlay_shell


def _imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names


def test_phase03_occ_preparation_modules_do_not_import_overlay_or_legacy_apply() -> None:
    occ_root = Path(sandbox.occ.__file__).resolve().parent
    phase03_files = [
        occ_root / "service.py",
        occ_root / "client.py",
        occ_root / "runtime_ops.py",
        occ_root / "changeset" / "builders.py",
        occ_root / "changeset" / "intent.py",
        occ_root / "changeset" / "types.py",
        occ_root / "orchestrator.py",
        occ_root / "content" / "gitignore_oracle.py",
        occ_root / "content" / "hashing.py",
        occ_root / "content" / "layer_backed_content.py",
        occ_root / "direct" / "merge.py",
        occ_root / "gated" / "merge.py",
        occ_root / "commit_transaction.py",
    ]

    forbidden = {
        "sandbox.overlay",
        "sandbox.occ.direct.direct_merge_coordinator",
        "sandbox.occ.gated.gated_coordinator",
        "sandbox.occ.merge",
        "sandbox.occ.routing",
    }
    hits: list[tuple[str, str]] = []
    for path in phase03_files:
        for name in _imports(path):
            if name in forbidden or any(name.startswith(f"{item}.") for item in forbidden):
                hits.append((path.name, name))

    assert hits == []


def test_capture_to_changeset_is_the_runtime_overlay_bridge() -> None:
    runtime_root = Path(sandbox.runtime.overlay_shell.__file__).resolve().parent
    imports = _imports(runtime_root / "capture_to_changeset.py")

    assert "sandbox.overlay.capture.changes" in imports
    assert "sandbox.occ.changeset.builders" in imports


def test_pipeline_is_the_only_runtime_bridge_to_occ_client() -> None:
    runtime_root = Path(sandbox.runtime.overlay_shell.__file__).resolve().parent
    imports = _imports(runtime_root / "pipeline.py")

    assert "sandbox.occ.client" in imports
    assert "sandbox.runtime.overlay_shell.capture_to_changeset" in imports
