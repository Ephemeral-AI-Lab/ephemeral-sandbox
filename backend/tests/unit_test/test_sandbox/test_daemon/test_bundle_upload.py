"""Unit tests for the sandbox runtime bundle.

The headline tests extract the bundle to a tmp dir and import the runtime
entrypoint from the extracted tree in a fresh subprocess. That mechanically
catches the "transitive-imports-not-bundled" failure mode the runtime would
otherwise hit on a clean sandbox image.
"""

from __future__ import annotations

import io
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock

import pytest

from sandbox.host.runtime_bundle import (
    BUNDLE_REMOTE_DIR,
    _ensure_runtime_uploaded_with_exec,
    _runtime_bundle_bytes,
    bundle_hash,
)


_BUNDLE_SIZE_BUDGET = 1 * 1024 * 1024  # 1 MB hard ceiling per spec


def _extract_bundle(bundle: bytes, target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        # Python 3.12 emits a DeprecationWarning when filter is omitted.
        tar.extractall(target, filter="data")


def test_bundle_size_under_budget() -> None:
    bundle = _runtime_bundle_bytes()
    assert len(bundle) > 0
    assert len(bundle) < _BUNDLE_SIZE_BUDGET, (
        f"runtime bundle is {len(bundle)} B, budget is {_BUNDLE_SIZE_BUDGET} B"
    )


def test_bundle_layout_includes_required_paths(tmp_path: Path) -> None:
    bundle = _runtime_bundle_bytes()
    extract_dir = tmp_path / "extracted"
    _extract_bundle(bundle, extract_dir)

    required = [
        "sandbox/__init__.py",
        "sandbox/api/__init__.py",
        "sandbox/api/facade.py",
        "sandbox/api/tool/__init__.py",
        "sandbox/contract/__init__.py",
        "sandbox/runtime/__init__.py",
        "sandbox/runtime/async_bridge.py",
        "sandbox/runtime/daemon/__main__.py",
        "sandbox/runtime/daemon/rpc/__init__.py",
        "sandbox/runtime/daemon/rpc/server.py",
        "sandbox/runtime/daemon/rpc/dispatcher.py",
        "sandbox/runtime/daemon/service/__init__.py",
        "sandbox/runtime/daemon/service/layer_stack_client.py",
        "sandbox/runtime/daemon/service/workspace_binding.py",
        "sandbox/runtime/daemon/handler/health.py",
        "sandbox/runtime/daemon/handler/workspace.py",
        "sandbox/runtime/daemon/service/workspace_server.py",
        "sandbox/runtime/daemon/service/shell_runner.py",
        "sandbox/runtime/daemon/handler/__init__.py",
        "sandbox/runtime/daemon/handler/request_context.py",
        "sandbox/runtime/daemon/handler/metrics.py",
        "sandbox/runtime/daemon/handler/overlay.py",
        "sandbox/runtime/daemon/handler/tools/__init__.py",
        "sandbox/runtime/daemon/handler/tools/edit.py",
        "sandbox/runtime/daemon/handler/tools/read.py",
        "sandbox/runtime/daemon/handler/tools/shell.py",
        "sandbox/runtime/daemon/handler/tools/write.py",
        "sandbox/runtime/daemon/service/occ_backend.py",
        "sandbox/command_exec/__init__.py",
        "sandbox/command_exec/contract/request.py",
        "sandbox/command_exec/contract/result.py",
        "sandbox/command_exec/contract/ports.py",
        "sandbox/command_exec/workspace/mount.py",
        "sandbox/command_exec/workspace/capture.py",
        "sandbox/layer_stack/workspace/base.py",
        "sandbox/overlay/cli.py",
        "sandbox/layer_stack/manifest/model.py",
        "sandbox/layer_stack/manifest/store.py",
        "sandbox/layer_stack/manager.py",
        "sandbox/layer_stack/workspace/binding.py",
        "sandbox/occ/capture/overlay.py",
        "sandbox/occ/result_projection.py",
        "sandbox/occ/changeset/builders.py",
        "sandbox/occ/changeset/prepared.py",
        "sandbox/occ/changeset/types.py",
        "sandbox/occ/commit_transaction.py",
        "sandbox/occ/routing/orchestrator.py",
        "sandbox/occ/content/gitignore_oracle.py",
        "sandbox/occ/content/hashing.py",
        "sandbox/occ/merge/direct.py",
        "sandbox/occ/merge/gated.py",
        "sandbox/overlay/capture/changes.py",
        "sandbox/overlay/capture/types.py",
        "sandbox/overlay/capture/upperdir.py",
        "sandbox/overlay/namespace/command.py",
        "sandbox/overlay/namespace/mounts.py",
        "sandbox/overlay/runner/runtime_invoker.py",
        "sandbox/overlay/runner/snapshot_overlay_runner.py",
    ]
    missing = [p for p in required if not (extract_dir / p).exists()]
    assert missing == [], f"bundle is missing required paths: {missing}"
    assert not (extract_dir / "sandbox/api/status.py").exists()
    assert not (extract_dir / "sandbox/api/tool/raw_exec.py").exists()
    assert not (extract_dir / "sandbox/api/tool/_payload.py").exists()
    assert not (extract_dir / "sandbox/host").exists()
    assert not (extract_dir / "sandbox/provider").exists()
    assert not (extract_dir / "sandbox/testing").exists()


def test_bundle_excludes_pycache_and_compiled() -> None:
    bundle = _runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        names = tar.getnames()
    assert all("__pycache__" not in n for n in names), (
        f"bundle contains __pycache__ entries: "
        f"{[n for n in names if '__pycache__' in n][:5]}"
    )
    assert all(not n.endswith((".pyc", ".pyo")) for n in names)


def test_bundle_excludes_host_and_public_transport_modules() -> None:
    bundle = _runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        names = set(tar.getnames())

    excluded = {
        "sandbox/api/status.py",
        "sandbox/api/tool/_payload.py",
        "sandbox/api/tool/edit.py",
        "sandbox/api/tool/raw_exec.py",
        "sandbox/api/tool/read.py",
        "sandbox/api/tool/shell.py",
        "sandbox/api/tool/write.py",
        "sandbox/host/runtime_bundle.py",
        "sandbox/host/daemon_client.py",
        "sandbox/provider/registry.py",
        "sandbox/provider/daytona/adapter.py",
    }
    assert excluded.isdisjoint(names)
    assert all(not name.startswith("sandbox/provider/") for name in names)
    assert all(not name.startswith("sandbox/host/") for name in names)
    assert all(not name.startswith("sandbox/testing/") for name in names)


def test_bundle_extracted_python_modules_import_clean(tmp_path: Path) -> None:
    bundle = _runtime_bundle_bytes()
    extract_dir = tmp_path / "extracted"
    _extract_bundle(bundle, extract_dir)

    modules = sorted(
        path.relative_to(extract_dir).with_suffix("").as_posix().replace("/", ".")
        for path in (extract_dir / "sandbox").rglob("*.py")
        if path.name != "__init__.py"
    )
    script = (
        "import importlib, sys; "
        f"sys.path.insert(0, {str(extract_dir)!r}); "
        f"modules = {modules!r}; "
        "[importlib.import_module(name) for name in modules]; "
        "print('imported', len(modules))"
    )
    result = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        timeout=30,
        env={"PATH": "/usr/bin:/bin"},
    )
    assert result.returncode == 0, (
        f"bundle module import failed: stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_bundle_includes_peer_setup_scripts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    src_root = tmp_path / "src"
    setup_script = src_root / "sandbox" / "runtime" / "daemon" / "peer" / "setup.sh"
    setup_script.parent.mkdir(parents=True)
    setup_script.write_text("#!/usr/bin/env bash\necho setup\n", encoding="utf-8")

    monkeypatch.setattr("sandbox.host.runtime_bundle._src_root", lambda: src_root)
    monkeypatch.setattr("sandbox.host.runtime_bundle._BUNDLE_CACHE", None)
    monkeypatch.setattr("sandbox.host.runtime_bundle._BUNDLE_HASH_CACHE", None)

    bundle = _runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        member = tar.extractfile("sandbox/runtime/daemon/peer/setup.sh")
        assert member is not None
        assert member.read().decode("utf-8") == "#!/usr/bin/env bash\necho setup\n"


def test_bundle_extracted_daemon_modules_import_clean(tmp_path: Path) -> None:
    bundle = _runtime_bundle_bytes()
    extract_dir = tmp_path / "extracted"
    _extract_bundle(bundle, extract_dir)

    cmd = [
        sys.executable,
        "-c",
        (
            f"import sys; sys.path.insert(0, {str(extract_dir)!r}); "
            "import asyncio; "
            "from sandbox.runtime.daemon.rpc.dispatcher import OP_TABLE, dispatch_envelope_async; "
            "response = asyncio.run(dispatch_envelope_async({'op':'missing'})); "
            "print('ok:', isinstance(OP_TABLE, dict), response['error']['kind'])"
        ),
    ]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=30,
        env={"PATH": "/usr/bin:/bin"},
    )
    assert result.returncode == 0, (
        f"daemon import failed: stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "ok: True unknown_op" in result.stdout


def test_bundle_hash_is_deterministic() -> None:
    a = bundle_hash()
    b = bundle_hash()
    assert a == b
    assert len(a) == 64


@pytest.mark.asyncio
async def test_ensure_runtime_uploaded_uploads_when_marker_missing() -> None:
    transport: Any = AsyncMock()
    # exec sequence: marker-check (miss) → setup → N chunk writes → finalize.
    # Stub all calls as success — count is bundle-dependent, so use a
    # repeating success response.
    transport.exec.return_value = type("R", (), {"exit_code": 0, "stdout": ""})()

    async def fake_marker_check(*args: Any, **kwargs: Any) -> Any:
        # First call is the marker check — return non-zero to force upload path.
        del args, kwargs
        transport.exec.side_effect = None  # subsequent calls use return_value
        return type("R", (), {"exit_code": 1, "stdout": ""})()

    transport.exec.side_effect = fake_marker_check
    digest = await _ensure_runtime_uploaded_with_exec("sb-1", transport.exec)
    assert digest == bundle_hash()
    # Marker-check + setup + ≥1 chunk + finalize == ≥4 calls.
    assert transport.exec.await_count >= 4

    # Bundle bytes are streamed via chunked exec, NOT write_bytes.
    transport.write_bytes.assert_not_awaited()

    # Last exec call must be the finalize — extracts the tarball and writes hash.
    finalize_cmd = transport.exec.await_args_list[-1].args[1]
    assert BUNDLE_REMOTE_DIR in finalize_cmd
    assert "tar -xzf" in finalize_cmd
    assert ".bundle-hash" in finalize_cmd

    # Chunk writes pipe ``printf`` through ``base64 -d`` straight into the
    # tarball — the previous ``.b64`` staging file is gone. Verify that
    # decode happens during streaming, not in the finalize step.
    chunk_cmds = [
        call.args[1] for call in transport.exec.await_args_list[2:-1]
    ]
    assert chunk_cmds, "expected at least one streaming chunk write"
    for cmd in chunk_cmds:
        assert "printf %s" in cmd
        assert "base64 -d" in cmd
        assert ".b64" not in cmd
    assert "base64 -d" not in finalize_cmd
    assert ".b64" not in finalize_cmd


@pytest.mark.asyncio
async def test_ensure_runtime_uploaded_no_op_when_hash_matches() -> None:
    transport: Any = AsyncMock()
    digest = bundle_hash()
    transport.exec.side_effect = [
        type("R", (), {"exit_code": 0, "stdout": digest + "\n"})(),
    ]
    out = await _ensure_runtime_uploaded_with_exec("sb-1", transport.exec)
    assert out == digest
    # Only the marker check ran; no upload.
    assert transport.exec.await_count == 1


@pytest.mark.asyncio
async def test_ensure_runtime_uploaded_raises_on_upload_failure() -> None:
    """When the finalize step fails, ensure_runtime_uploaded raises clean."""
    transport: Any = AsyncMock()
    call_index = {"i": 0}

    async def script(*args: Any, **kwargs: Any) -> Any:
        del kwargs
        i = call_index["i"]
        call_index["i"] += 1
        cmd = args[1] if len(args) > 1 else ""
        # 0: marker-check (miss) → exit 1
        if i == 0:
            return type("R", (), {"exit_code": 1, "stdout": ""})()
        # Last call is the finalize (contains "tar -xzf"); fail it.
        if "tar -xzf" in cmd:
            return type(
                "R", (), {"exit_code": 2, "stdout": "tar: not enough disk space"}
            )()
        return type("R", (), {"exit_code": 0, "stdout": ""})()

    transport.exec.side_effect = script
    with pytest.raises(RuntimeError, match="runtime bundle upload failed"):
        await _ensure_runtime_uploaded_with_exec("sb-broken", transport.exec)


@pytest.mark.asyncio
async def test_ensure_runtime_uploaded_re_uploads_when_hash_mismatches() -> None:
    transport: Any = AsyncMock()
    call_index = {"i": 0}

    async def script(*args: Any, **kwargs: Any) -> Any:
        del args, kwargs
        i = call_index["i"]
        call_index["i"] += 1
        # First call is the marker check; return a stale hash.
        if i == 0:
            return type(
                "R",
                (),
                {"exit_code": 0, "stdout": "stale-hash-from-prior-deploy\n"},
            )()
        return type("R", (), {"exit_code": 0, "stdout": ""})()

    transport.exec.side_effect = script
    digest = await _ensure_runtime_uploaded_with_exec("sb-1", transport.exec)
    assert digest == bundle_hash()
    # Marker-check + setup + chunks + finalize ≥ 4 calls.
    assert transport.exec.await_count >= 4
    transport.write_bytes.assert_not_awaited()
