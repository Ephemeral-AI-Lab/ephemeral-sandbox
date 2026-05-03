"""Unit tests for ``sandbox.code_intelligence.daemon.launcher``.

The headline tests extract the bundle to a tmp dir and import the daemon
entrypoint from the extracted tree in a fresh subprocess. That mechanically
catches the "transitive-imports-not-bundled" failure mode the daemon would
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

from sandbox.code_intelligence.daemon.launcher import (
    BUNDLE_REMOTE_DIR,
    _runtime_bundle_bytes,
    bundle_hash,
    ensure_runtime_uploaded,
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
        "sandbox/api/transport.py",
        "sandbox/api/bash.py",
        "sandbox/api/models.py",
        "sandbox/client/async_bridge.py",
        "sandbox/code_intelligence/service.py",
        "sandbox/code_intelligence/backends/protocol.py",
        "sandbox/code_intelligence/daemon/__main__.py",
        "sandbox/code_intelligence/daemon/server.py",
        "sandbox/code_intelligence/daemon/protocol.py",
        "sandbox/code_intelligence/daemon/storage.py",
        "msgpack/__init__.py",
    ]
    missing = [p for p in required if not (extract_dir / p).exists()]
    assert missing == [], f"bundle is missing required paths: {missing}"


def test_bundle_excludes_pycache_and_compiled(tmp_path: Path) -> None:
    bundle = _runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(bundle), mode="r:gz") as tar:
        names = tar.getnames()
    assert all("__pycache__" not in n for n in names), (
        f"bundle contains __pycache__ entries: "
        f"{[n for n in names if '__pycache__' in n][:5]}"
    )
    assert all(not n.endswith((".pyc", ".pyo")) for n in names)


def test_bundle_extracted_daemon_imports_clean(tmp_path: Path) -> None:
    bundle = _runtime_bundle_bytes()
    extract_dir = tmp_path / "extracted"
    _extract_bundle(bundle, extract_dir)

    cmd = [
        sys.executable,
        "-c",
        (
            f"import sys; sys.path.insert(0, {str(extract_dir)!r}); "
            "from sandbox.code_intelligence.daemon.__main__ import main; "
            "from sandbox.code_intelligence.daemon.server import DISPATCH; "
            "print('ok:', callable(main), sorted(DISPATCH))"
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
    assert "ok: True" in result.stdout
    assert "ping" in result.stdout


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
    digest = await ensure_runtime_uploaded(transport, "sb-1")
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
    out = await ensure_runtime_uploaded(transport, "sb-1")
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
        await ensure_runtime_uploaded(transport, "sb-broken")


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
    digest = await ensure_runtime_uploaded(transport, "sb-1")
    assert digest == bundle_hash()
    # Marker-check + setup + chunks + finalize ≥ 4 calls.
    assert transport.exec.await_count >= 4
    transport.write_bytes.assert_not_awaited()
