"""Verify the runtime bundle contains sandbox/plugin/* so the daemon can
import sandbox.plugin.runtime in-sandbox."""

from __future__ import annotations

import gzip
import io
import tarfile

from sandbox.host import runtime_bundle


def test_bundle_contains_sandbox_plugin_runtime() -> None:
    runtime_bundle._BUNDLE_CACHE = None  # force a fresh build
    runtime_bundle._BUNDLE_HASH_CACHE = None
    bundle = runtime_bundle._runtime_bundle_bytes()
    raw = gzip.GzipFile(fileobj=io.BytesIO(bundle), mode="rb").read()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tar:
        names = set(tar.getnames())
    assert any(
        name.startswith("sandbox/plugin/") and name.endswith(".py")
        for name in names
    ), f"runtime bundle missing sandbox/plugin/*: {sorted(names)[:20]}"
    assert "sandbox/plugin/op_context.py" in names
    assert "sandbox/plugin/op_registry.py" in names
    assert "sandbox/plugin/overlay_child.py" in names
    assert "sandbox/plugin/overlay_dispatch.py" in names
    assert "sandbox/plugin/runtime/__init__.py" in names
    assert "sandbox/plugin/runtime/registry.py" not in names
    assert "sandbox/plugin/runtime/context.py" not in names
    assert "sandbox/plugin/handler.py" in names
