"""Dependency-boundary tests for snapshot-overlay support modules."""

from __future__ import annotations

from pathlib import Path
import tarfile
import io

import sandbox.overlay
from sandbox.host.runtime_bundle import _runtime_bundle_bytes


def _overlay_modules() -> list[Path]:
    overlay_root = Path(sandbox.overlay.__file__).resolve().parent
    return sorted(overlay_root.glob("*.py"))


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
    overlay_root = Path(sandbox.overlay.__file__).resolve().parent
    for rel in (
        "overlay_layer_manager.py",
        "overlay_client.py",
        "overlay_occ.py",
    ):
        assert not (overlay_root / rel).exists()


def test_runtime_bundle_contains_unified_snapshot_runtime_without_ndjson() -> None:
    raw = _runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "sandbox/overlay/capture.py" in names
    assert "sandbox/overlay/path_change.py" in names
    assert "sandbox/_shared/shell_contract.py" in names
    assert "sandbox/ephemeral_workspace/_execute_command.py" not in names
    assert "sandbox/overlay/namespace_runner.py" in names
    assert "sandbox/overlay/namespace_entrypoint.py" in names
    assert "sandbox/overlay/mount_syscalls.py" in names
    assert "sandbox/overlay/writable_dirs.py" in names
    assert "sandbox/daemon/handler/overlay.py" not in names
    assert "sandbox/overlay_request.py" not in names
    assert "sandbox/overlay_result.py" not in names
    assert "sandbox/workspace_capture.py" not in names
    assert "sandbox/workspace_mount.py" not in names
    assert "sandbox/overlay_worker.py" not in names
    assert "sandbox/overlay_mounts.py" not in names
    assert "sandbox/overlay_runner.py" not in names
    assert "sandbox/overlay_pipeline.py" not in names
    assert "sandbox/layer_stack/manifest.py" in names
    assert "sandbox/overlay/cli.py" not in names
    assert "sandbox/overlay/invoker.py" not in names
    assert "sandbox/overlay/factory.py" not in names
    assert "sandbox/overlay/command.py" not in names
    assert "sandbox/overlay/change_synthesis.py" not in names
    assert "sandbox/overlay/layout.py" not in names
    assert all(not name.startswith("sandbox/host/") for name in names)
    assert all(not name.startswith("sandbox/provider/") for name in names)
    assert all(not name.startswith("sandbox/testing/") for name in names)
