"""Dependency boundary checks for Phase 03 OCC preparation."""

from __future__ import annotations

import ast
from pathlib import Path

import sandbox.occ


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
        occ_root / "router.py",
        occ_root / "changeset" / "builders.py",
        occ_root / "changeset" / "prepared.py",
        occ_root / "changeset" / "types.py",
        occ_root / "content" / "gitignore_oracle.py",
        occ_root / "content" / "hashing.py",
        occ_root / "stage" / "direct.py",
        occ_root / "stage" / "gated.py",
        occ_root / "stage" / "transaction.py",
        occ_root / "commit_queue.py",
    ]

    forbidden = {
        "sandbox.execution.overlay",
        "sandbox.occ.stage.direct.direct_merge_coordinator",
        "sandbox.occ.stage.gated.gated_coordinator",
    }
    hits: list[tuple[str, str]] = []
    for path in phase03_files:
        for name in _imports(path):
            if name in forbidden or any(name.startswith(f"{item}.") for item in forbidden):
                hits.append((path.name, name))

    assert hits == []


def test_overlay_capture_module_is_the_occ_overlay_bridge() -> None:
    occ_root = Path(sandbox.occ.__file__).resolve().parent
    imports = _imports(occ_root / "overlay.py")

    assert "sandbox.execution.overlay.change" in imports
    assert "sandbox.occ.changeset.builders" in imports


def test_overlay_capture_conversion_does_not_import_occ_service() -> None:
    occ_root = Path(sandbox.occ.__file__).resolve().parent
    imports = _imports(occ_root / "overlay.py")

    assert "sandbox.occ.service" not in imports
    assert not (occ_root / "overlay_capture.py").exists()
