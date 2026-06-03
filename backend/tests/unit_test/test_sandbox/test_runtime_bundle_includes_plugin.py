"""Verify the Rust runtime payload contains the current plugin bridge modules."""

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
    assert any(name.startswith("plugins/catalog/lsp/runtime/") for name in names)
    assert "sandbox/ephemeral_workspace/plugin/op_context.py" in names
    assert "sandbox/ephemeral_workspace/plugin/op_registry.py" in names
    assert "sandbox/ephemeral_workspace/plugin/ppc_service.py" in names
    assert "sandbox/shared/models.py" in names
    assert "sandbox/shared/command_exec_contract.py" in names
    assert "plugins/catalog/lsp/runtime/server.py" in names
    assert "plugins/catalog/lsp/runtime/pyright_session.py" in names
    assert not any(name.startswith("sandbox/daemon/") for name in names)
    assert not any(name.startswith("sandbox/overlay/") for name in names)
    assert not any(name.startswith("sandbox/occ/") for name in names)
    assert not any(name.startswith("sandbox/layer_stack/") for name in names)
    assert not any(name.startswith("sandbox/isolated_workspace/") for name in names)
