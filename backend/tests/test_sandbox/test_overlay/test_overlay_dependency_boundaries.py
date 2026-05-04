"""Dependency-boundary tests for the Phase 02 overlay modules."""

from __future__ import annotations

from pathlib import Path
import tarfile
import io

import sandbox.overlay
import sandbox.runtime.overlay_shell
from sandbox.overlay.runner.runtime_bundle import snapshot_overlay_runtime_bundle_bytes


def test_phase02_overlay_modules_do_not_import_occ_or_git_policy() -> None:
    overlay_root = Path(sandbox.overlay.__file__).resolve().parent
    runtime_root = Path(sandbox.runtime.overlay_shell.__file__).resolve().parent
    checked_roots = (
        overlay_root / "capture",
        overlay_root / "namespace",
        overlay_root / "runner",
        runtime_root,
    )

    forbidden = (
        "sandbox.occ",
        "git check-ignore",
        "gitignore",
        "publish_layer",
        "publish_changes",
    )
    hits: list[str] = []
    for root in checked_roots:
        for path in root.rglob("*.py"):
            text = path.read_text(encoding="utf-8")
            for token in forbidden:
                if token in text:
                    hits.append(f"{path.relative_to(overlay_root.parent)}: {token}")

    assert hits == []


def test_phase02_forbidden_overlay_modules_do_not_exist() -> None:
    overlay_root = Path(sandbox.overlay.__file__).resolve().parent

    for rel in (
        "layer_manager.py",
        "capture/ndjson.py",
        "occ.py",
    ):
        assert not (overlay_root / rel).exists()


def test_phase02_runtime_bundle_contains_snapshot_runtime_without_ndjson() -> None:
    raw = snapshot_overlay_runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "sandbox/runtime/overlay_shell/cli.py" in names
    assert "sandbox/overlay/capture/upperdir.py" in names
    assert "sandbox/overlay/namespace/mounts.py" in names
    assert "sandbox/layer_stack/manifest.py" in names
    assert "sandbox/overlay/capture/ndjson.py" not in names
