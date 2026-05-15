"""Dependency-boundary tests for snapshot-overlay support modules."""

from __future__ import annotations

from pathlib import Path
import tarfile
import io

import sandbox.execution
from sandbox.host.runtime_bundle import _runtime_bundle_bytes


def _overlay_modules() -> list[Path]:
    execution_root = Path(sandbox.execution.__file__).resolve().parent
    return sorted(execution_root.glob("overlay_*.py"))


def test_phase02_overlay_modules_do_not_import_occ_or_git_policy() -> None:
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


def test_phase02_forbidden_overlay_modules_do_not_exist() -> None:
    execution_root = Path(sandbox.execution.__file__).resolve().parent
    for rel in (
        "overlay_layer_manager.py",
        "overlay_client.py",
        "overlay_occ.py",
    ):
        assert not (execution_root / rel).exists()


def test_runtime_bundle_contains_unified_snapshot_runtime_without_ndjson() -> None:
    raw = _runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "sandbox/execution/overlay_capture.py" in names
    assert "sandbox/execution/overlay_change.py" in names
    assert "sandbox/execution/contract.py" in names
    assert "sandbox/execution/orchestrator.py" in names
    assert "sandbox/daemon/handler/overlay.py" in names
    assert "sandbox/execution/overlay_request.py" not in names
    assert "sandbox/execution/overlay_result.py" not in names
    assert "sandbox/execution/workspace_capture.py" not in names
    assert "sandbox/execution/workspace_mount.py" not in names
    assert "sandbox/execution/overlay_worker.py" not in names
    assert "sandbox/execution/overlay_mounts.py" not in names
    assert "sandbox/execution/overlay_runner.py" not in names
    assert "sandbox/execution/overlay_pipeline.py" not in names
    assert "sandbox/layer_stack/manifest.py" in names
    assert "sandbox/overlay/cli.py" not in names
    assert "sandbox/overlay/invoker.py" not in names
    assert "sandbox/overlay/factory.py" not in names
    assert "sandbox/overlay/command.py" not in names
    assert all(not name.startswith("sandbox/host/") for name in names)
    assert all(not name.startswith("sandbox/provider/") for name in names)
    assert all(not name.startswith("sandbox/testing/") for name in names)
