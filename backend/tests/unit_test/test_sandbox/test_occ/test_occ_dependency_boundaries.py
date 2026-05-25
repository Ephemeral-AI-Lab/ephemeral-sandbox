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
        occ_root / "changeset_preparation.py",
        occ_root / "changeset.py",
        occ_root / "gitignore.py",
        occ_root / "content_hashing.py",
        occ_root / "path_staging.py",
        occ_root / "commit_transaction.py",
        occ_root / "commit_queue.py",
    ]

    forbidden_exact = {
        "sandbox.occ.path_staging.direct_merge_coordinator",
        "sandbox.occ.path_staging.gated_coordinator",
    }
    forbidden_prefix = ("sandbox.overlay_",)
    hits: list[tuple[str, str]] = []
    for path in phase03_files:
        for name in _imports(path):
            if name in forbidden_exact or any(
                name.startswith(prefix) for prefix in forbidden_prefix
            ):
                if name == "sandbox.overlay.path_change":
                    # The overlay→OCC bridge is the only allowed overlay import for OCC.
                    continue
                hits.append((path.name, name))

    assert hits == []


def test_overlay_capture_module_is_the_occ_overlay_bridge() -> None:
    occ_root = Path(sandbox.occ.__file__).resolve().parent
    imports = _imports(occ_root / "overlay_change_conversion.py")

    assert "sandbox.overlay.path_change" in imports
    assert "sandbox.occ.changeset" in imports


def test_overlay_capture_conversion_does_not_import_occ_service() -> None:
    occ_root = Path(sandbox.occ.__file__).resolve().parent
    imports = _imports(occ_root / "overlay_change_conversion.py")

    assert "sandbox.occ.service" not in imports
    assert not (occ_root / "overlay.py").exists()
    assert not (occ_root / "overlay_capture.py").exists()


def test_occ_package_root_does_not_reexport_concrete_contracts() -> None:
    assert not hasattr(sandbox.occ, "OccService")
    assert not hasattr(sandbox.occ, "CommitQueue")
    assert not hasattr(sandbox.occ, "Change")
