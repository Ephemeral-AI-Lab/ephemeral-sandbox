"""Verify the runtime bundle contains current ephemeral plugin modules."""

from __future__ import annotations

import gzip
import io
import tarfile

from sandbox.host import runtime_bundle


def test_bundle_contains_sandbox_plugin_modules() -> None:
    runtime_bundle._BUNDLE_CACHE = None  # force a fresh build
    runtime_bundle._BUNDLE_HASH_CACHE = None
    bundle = runtime_bundle._runtime_bundle_bytes()
    raw = gzip.GzipFile(fileobj=io.BytesIO(bundle), mode="rb").read()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
        names = set(tar.getnames())
    assert any(
        name.startswith("sandbox/ephemeral_workspace/plugin/") and name.endswith(".py")
        for name in names
    ), f"runtime bundle missing ephemeral plugin modules: {sorted(names)[:20]}"
    assert "sandbox/ephemeral_workspace/plugin/op_context.py" in names
    assert "sandbox/ephemeral_workspace/plugin/op_registry.py" in names
    assert "sandbox/ephemeral_workspace/plugin/overlay_child.py" in names
    assert "sandbox/ephemeral_workspace/plugin/overlay_dispatch.py" in names
    assert "sandbox/ephemeral_workspace/plugin/handler.py" in names
