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

from sandbox.daemon_paths import BUNDLE_REMOTE_DIR, BUNDLE_REMOTE_TARBALL
from sandbox.host.runtime_bundle import (
    _ensure_runtime_uploaded_with_exec,
    _runtime_bundle_bytes,
    bundle_hash,
    clear_bundle_caches,
    compute_bundle_hash,
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
        "sandbox/daemon/async_bridge.py",
        "sandbox/daemon_paths.py",
        "sandbox/_shared/models.py",
        "sandbox/timing.py",
        "sandbox/timing_keys.py",
        "sandbox/daemon/__init__.py",
        "sandbox/daemon/__main__.py",
        "sandbox/daemon/rpc/__init__.py",
        "sandbox/daemon/rpc/server.py",
        "sandbox/daemon/rpc/dispatcher.py",
        "sandbox/daemon/_toolbox.py",
        "sandbox/daemon/_wire.py",
        "sandbox/daemon/occ_backend.py",
        "sandbox/daemon/workspace_server.py",
        "sandbox/daemon/service/__init__.py",
        "sandbox/daemon/service/layer_stack_client.py",
        "sandbox/daemon/service/workspace_binding.py",
        "sandbox/daemon/handler/health.py",
        "sandbox/daemon/handler/workspace.py",
        "sandbox/daemon/service/shell_runner.py",
        "sandbox/daemon/handler/__init__.py",
        "sandbox/daemon/handler/metrics.py",
        "sandbox/daemon/handler/overlay.py",
        "sandbox/daemon/handler/edit.py",
        "sandbox/daemon/handler/read.py",
        "sandbox/daemon/handler/write.py",
        "sandbox/daemon/scripts/__init__.py",
        "sandbox/daemon/scripts/install_git.sh",
        "sandbox/daemon/scripts/launch_daemon.sh",
        "sandbox/daemon/scripts/thin_client.py",
        "sandbox/execution/__init__.py",
        "sandbox/execution/orchestrator.py",
        "sandbox/execution/env_policy.py",
        "sandbox/execution/namespace_child.py",
        "sandbox/execution/contract.py",
        "sandbox/execution/strategy_base.py",
        "sandbox/execution/strategy_copy_backed.py",
        "sandbox/execution/strategy_private_namespace.py",
        "sandbox/layer_stack/workspace_base.py",
        "sandbox/execution/path_change.py",
        "sandbox/execution/overlay_capture.py",
        "sandbox/layer_stack/manifest.py",
        "sandbox/layer_stack/stack.py",
        "sandbox/layer_stack/workspace_binding.py",
        "sandbox/occ/overlay.py",
        "sandbox/occ/changeset.py",
        "sandbox/occ/commit_queue.py",
        "sandbox/occ/commit_transaction.py",
        "sandbox/occ/gitignore.py",
        "sandbox/occ/hashing.py",
        "sandbox/occ/preparer.py",
        "sandbox/occ/stage.py",
        "sandbox/occ/stage_policy.py",
    ]
    missing = [p for p in required if not (extract_dir / p).exists()]
    assert missing == [], f"bundle is missing required paths: {missing}"
    removed = [
        "sandbox/overlay/capture/changes.py",
        "sandbox/overlay/capture/types.py",
        "sandbox/overlay/capture/upperdir.py",
        "sandbox/overlay/namespace/command.py",
        "sandbox/overlay/namespace/mounts.py",
        "sandbox/overlay/runner/runtime_invoker.py",
        "sandbox/overlay/runner/snapshot_overlay_runner.py",
        "sandbox/execution/overlay_mounts.py",
        "sandbox/execution/overlay_pipeline.py",
        "sandbox/execution/overlay_request.py",
        "sandbox/execution/overlay_result.py",
        "sandbox/execution/overlay_runner.py",
        "sandbox/execution/overlay_worker.py",
        "sandbox/execution/workspace_capture.py",
        "sandbox/execution/workspace_mount.py",
        "sandbox/occ/changeset/prepared.py",
        "sandbox/occ/changeset/types.py",
        "sandbox/occ/content/gitignore_oracle.py",
        "sandbox/occ/content/hashing.py",
        "sandbox/occ/stage/__init__.py",
        "sandbox/occ/stage/direct.py",
        "sandbox/occ/stage/gated.py",
        "sandbox/occ/stage/policy.py",
        "sandbox/occ/stage/transaction.py",
        "sandbox/occ/timing_keys.py",
        "sandbox/daemon/handler/request_context.py",
        "sandbox/daemon/service/occ_backend.py",
        "sandbox/daemon/service/result_projection.py",
        "sandbox/daemon/service/workspace_server.py",
    ]
    present_removed = [p for p in removed if (extract_dir / p).exists()]
    assert present_removed == []
    assert not (extract_dir / "sandbox/api/status.py").exists()
    assert not (extract_dir / "sandbox/api/tool/raw_exec.py").exists()
    assert not (extract_dir / "sandbox/api/tool/_payload.py").exists()
    assert not (extract_dir / "sandbox/contract").exists()
    assert not (extract_dir / "sandbox/host").exists()
    assert not (extract_dir / "sandbox/provider").exists()
    assert not (extract_dir / "sandbox/testing").exists()


def test_bundle_excludes_pycache_and_compiled() -> None:
    bundle = _runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        names = tar.getnames()
    assert all("__pycache__" not in n for n in names), (
        f"bundle contains __pycache__ entries: {[n for n in names if '__pycache__' in n][:5]}"
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
    setup_script = src_root / "sandbox" / "daemon" / "peer" / "setup.sh"
    setup_script.parent.mkdir(parents=True)
    setup_script.write_text("#!/usr/bin/env bash\necho setup\n", encoding="utf-8")

    monkeypatch.setattr("sandbox.host.runtime_bundle._src_root", lambda: src_root)
    clear_bundle_caches()

    try:
        bundle = _runtime_bundle_bytes()
        with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
            member = tar.extractfile("sandbox/daemon/peer/setup.sh")
            assert member is not None
            assert member.read().decode("utf-8") == "#!/usr/bin/env bash\necho setup\n"
    finally:
        clear_bundle_caches()


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
            "from sandbox.daemon.rpc.dispatcher import OP_TABLE, dispatch_envelope_async; "
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
    assert compute_bundle_hash(b"abc") != compute_bundle_hash(b"abcd")


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
    assert "mv -f" not in finalize_cmd
    assert f"tar -xzf {BUNDLE_REMOTE_TARBALL} " not in finalize_cmd
    assert ".staging" in finalize_cmd
    assert ".bundle-hash" in finalize_cmd

    # Chunk writes pipe ``printf`` through ``base64 -d`` straight into the
    # tarball — the previous ``.b64`` staging file is gone. Verify that
    # decode happens during streaming, not in the finalize step.
    chunk_cmds = [call.args[1] for call in transport.exec.await_args_list[2:-1]]
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
            return type("R", (), {"exit_code": 2, "stdout": "tar: not enough disk space"})()
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
