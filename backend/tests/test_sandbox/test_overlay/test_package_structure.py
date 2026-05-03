"""Overlay package boundary tests for Slice 5b relocation."""

from __future__ import annotations

from pathlib import Path

import sandbox.occ
import sandbox.overlay


def _overlay_root() -> Path:
    return Path(sandbox.overlay.__file__).resolve().parent


def _occ_root() -> Path:
    return Path(sandbox.occ.__file__).resolve().parent


def test_overlay_root_contains_only_target_layout_entries() -> None:
    expected = {
        "__init__.py",
        "bootstrap.py",
        "client.py",
        "config.py",
        "engine",
        "handlers",
        "runtime",
        "setup.sh",
        "types.py",
        "wire.py",
    }

    actual = {
        path.name
        for path in _overlay_root().iterdir()
        if path.name not in {"__pycache__", ".DS_Store"}
    }

    assert actual == expected


def test_overlay_shim_files_do_not_exist() -> None:
    forbidden = {
        "capture_runner.py",
        "daemon_local.py",
        "process_exec.py",
        "run.py",
        "support.py",
    }

    assert forbidden.isdisjoint({path.name for path in _overlay_root().iterdir()})


def test_overlay_and_occ_do_not_import_each_other() -> None:
    overlay_hits = _grep_imports(_overlay_root(), "sandbox.occ")
    occ_hits = _grep_imports(_occ_root(), "sandbox.overlay")

    assert overlay_hits == []
    assert occ_hits == []


def test_old_code_intelligence_overlay_package_is_gone() -> None:
    assert not (_overlay_root().parent / "code_intelligence").exists()


def _grep_imports(root: Path, token: str) -> list[Path]:
    hits: list[Path] = []
    for path in root.rglob("*.py"):
        text = path.read_text(encoding="utf-8")
        if f"from {token}" in text or f"import {token}" in text:
            hits.append(path.relative_to(root))
    return hits
