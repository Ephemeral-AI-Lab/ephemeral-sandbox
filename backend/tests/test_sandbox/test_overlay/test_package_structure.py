"""Overlay package boundary tests for the Phase 02 snapshot layout."""

from __future__ import annotations

from pathlib import Path

import sandbox.occ
import sandbox.overlay.client


def _overlay_root() -> Path:
    return Path(sandbox.overlay.client.__file__).resolve().parent


def _occ_root() -> Path:
    return Path(sandbox.occ.__file__).resolve().parent


def test_overlay_contains_only_target_layout_files() -> None:
    expected = {
        "client.py",
        "capture/changes.py",
        "capture/upperdir.py",
        "handlers/run.py",
        "handlers/shell.py",
        "namespace/command.py",
        "namespace/mounts.py",
        "runner/runtime_bundle.py",
        "runner/runtime_invoker.py",
        "runner/snapshot_overlay_runner.py",
    }

    actual = _source_files(_overlay_root())

    assert actual == expected


def test_legacy_overlay_capture_runtime_is_removed() -> None:
    runtime_root = _overlay_root().parent / "runtime"

    assert _source_files(runtime_root / "overlay_capture") == set()
    assert _source_files(runtime_root / "overlay_capture_runtime") == set()


def test_overlay_shim_files_do_not_exist() -> None:
    forbidden = {
        "bootstrap.py",
        "capture_runner.py",
        "config.py",
        "daemon_local.py",
        "engine",
        "process_exec.py",
        "run.py",
        "runtime",
        "setup.sh",
        "support.py",
        "types.py",
        "wire.py",
    }

    assert forbidden.isdisjoint(_source_entries(_overlay_root()))


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


def _source_entries(root: Path) -> set[str]:
    entries: set[str] = set()
    for path in root.iterdir():
        if path.name in {"__pycache__", ".DS_Store"}:
            continue
        if path.is_dir() and not any(
            "__pycache__" not in child.parts and child.name != ".DS_Store"
            for child in path.rglob("*")
        ):
            continue
        entries.add(path.name)
    return entries


def _source_files(root: Path) -> set[str]:
    files: set[str] = set()
    for path in root.rglob("*"):
        if "__pycache__" in path.parts or path.name == ".DS_Store":
            continue
        if path.is_file():
            files.add(path.relative_to(root).as_posix())
    return files
