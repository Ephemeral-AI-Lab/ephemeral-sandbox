"""Dependency-boundary tests for snapshot-overlay support modules."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

import sandbox.overlay
from sandbox.host.runtime_bundle import _runtime_bundle_bytes


def _overlay_modules() -> list[Path]:
    overlay_root = Path(sandbox.overlay.__file__).resolve().parent
    return sorted(overlay_root.glob("*.py"))


def test_overlay_modules_do_not_import_occ_or_git_policy() -> None:
    forbidden = (
        "sandbox.occ",
        "git check-ignore",
        "gitignore",
        "publish_layer",
        "publish_changes",
    )
    hits: list[str] = []
    for path in _overlay_modules():
        text = path.read_text(encoding="utf-8")
        for token in forbidden:
            if token in text:
                hits.append(f"{path.name}: {token}")

    assert hits == []


def test_runtime_bundle_contains_overlay_runtime_boundary() -> None:
    raw = _runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "sandbox/overlay/capture.py" in names
    assert "sandbox/overlay/path_change.py" in names
    assert "sandbox/_shared/command_exec_contract.py" in names
    assert "sandbox/overlay/namespace_runner.py" in names
    assert "sandbox/overlay/namespace_entrypoint.py" in names
    assert "sandbox/overlay/mount_syscalls.py" in names
    assert "sandbox/overlay/writable_dirs.py" in names
    assert "sandbox/layer_stack/manifest.py" in names
    assert all(not name.startswith("sandbox/host/") for name in names)
    assert all(not name.startswith("sandbox/provider/") for name in names)
    assert all(not name.startswith("sandbox/testing/") for name in names)
