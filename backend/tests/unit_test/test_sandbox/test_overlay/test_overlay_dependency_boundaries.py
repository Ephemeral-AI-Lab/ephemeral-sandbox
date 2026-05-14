"""Dependency-boundary tests for the Phase 02 overlay modules."""

from __future__ import annotations

from pathlib import Path
import tarfile
import io

import sandbox.execution.overlay
from sandbox.host.runtime_bundle import _runtime_bundle_bytes


def _overlay_root() -> Path:
    return Path(sandbox.execution.overlay.__file__).resolve().parent


def test_phase02_overlay_modules_do_not_import_occ_or_git_policy() -> None:
    overlay_root = _overlay_root()
    checked_roots = (overlay_root,)

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
    overlay_root = _overlay_root()

    for rel in (
        "layer_manager.py",
        "capture/ndjson.py",
        "client.py",
        "occ.py",
    ):
        assert not (overlay_root / rel).exists()


def test_phase02_runtime_bundle_contains_snapshot_runtime_without_ndjson() -> None:
    raw = _runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "sandbox/execution/overlay/worker.py" in names
    assert "sandbox/execution/overlay/capture.py" in names
    assert "sandbox/execution/overlay/change.py" in names
    assert "sandbox/execution/overlay/result.py" in names
    assert "sandbox/daemon/handler/overlay.py" in names
    assert "sandbox/execution/overlay/mounts.py" in names
    assert "sandbox/execution/overlay/runner.py" in names
    assert "sandbox/execution/overlay/pipeline.py" in names
    assert "sandbox/layer_stack/manifest.py" in names
    assert "sandbox/overlay/cli.py" not in names
    assert "sandbox/overlay/invoker.py" not in names
    assert "sandbox/overlay/factory.py" not in names
    assert "sandbox/overlay/command.py" not in names
    assert "sandbox/execution/overlay/capture/ndjson.py" not in names
    assert "sandbox/execution/overlay/capture/upperdir.py" not in names
    assert "sandbox/execution/overlay/namespace/mounts.py" not in names
    assert "sandbox/execution/overlay/runner/snapshot_overlay_runner.py" not in names
    assert all(not name.startswith("sandbox/host/") for name in names)
    assert all(not name.startswith("sandbox/provider/") for name in names)
    assert all(not name.startswith("sandbox/testing/") for name in names)
