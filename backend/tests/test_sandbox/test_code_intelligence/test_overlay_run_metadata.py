"""Metadata and bundle tests for the capture-only overlay runtime."""

from __future__ import annotations

import io
import tarfile

from sandbox.overlay.engine.runtime_bundle import overlay_runtime_bundle_bytes
from sandbox.overlay.runtime.mounts import _NS_ROOT, _NS_TMP, _NS_UPPER


def test_namespace_mount_root_uses_writable_tmp_prefix() -> None:
    assert _NS_ROOT.startswith("/tmp/")
    assert _NS_TMP.startswith(_NS_ROOT)
    assert _NS_UPPER.startswith(_NS_TMP)


def test_runtime_bundle_contains_only_capture_runtime_modules() -> None:
    raw = overlay_runtime_bundle_bytes()

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "overlay_runtime/cli.py" in names
    assert "overlay_runtime/capture.py" in names
    assert "overlay_runtime/command.py" in names
    assert "overlay_runtime/mounts.py" in names
    assert "overlay_runtime/ndjson.py" in names
    assert "overlay_runtime/types.py" in names
    assert "overlay_runtime/classifier.py" not in names
    assert "overlay_runtime/gitignore.py" not in names
    assert "overlay_runtime/direct_routes.py" not in names
