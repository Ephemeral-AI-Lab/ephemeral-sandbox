"""Phase 02 snapshot-overlay runner tests."""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

from sandbox.layer_stack import WriteLayerChange, LayerStackManager
from sandbox.overlay import OverlayCapture, OverlayShellRequest, OverlaySnapshotRunner
from sandbox.runtime.daemon.rpc.dispatcher import dispatch_envelope_async


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_overlay_capture_timings_are_immutable() -> None:
    capture = OverlayCapture(
        exit_code=0,
        stdout_ref="/tmp/stdout",
        stderr_ref="/tmp/stderr",
        snapshot_version=1,
        changes=(),
        timings={"phase": 1.0},
    )

    with pytest.raises(TypeError):
        capture.timings["phase"] = 2.0


@pytest.mark.asyncio
async def test_snapshot_runner_executes_against_leased_manifest_without_publish(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "value.txt", b"old\n"),
            )
        ]
    )
    runner = OverlaySnapshotRunner(manager)
    request = OverlayShellRequest(
        request_id="request-a",
        command=(
            "bash",
            "-lc",
            "printf 'new\\n' > pkg/value.txt; printf out; printf err >&2",
        ),
        cwd=".",
        env={},
        timeout_seconds=5,
    )

    envelope = await runner.shell(request)

    assert envelope.exit_code == 0
    assert envelope.snapshot_version == 1
    assert Path(envelope.stdout_ref).read_text(encoding="utf-8") == "out"
    assert Path(envelope.stderr_ref).read_text(encoding="utf-8") == "err"
    assert manager.read_text("pkg/value.txt") == ("old\n", True)
    assert manager.pinned_layers() == ()

    assert len(envelope.changes) == 1
    change = envelope.changes[0]
    assert change.path == "pkg/value.txt"
    assert change.kind == "write"
    assert change.content_path is not None
    assert Path(change.content_path).read_bytes() == b"new\n"
    assert change.final_hash == hashlib.sha256(b"new\n").hexdigest()


@pytest.mark.asyncio
async def test_snapshot_runner_releases_lease_when_runtime_fails(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "value.txt", b"old\n"),
            )
        ]
    )

    class _FailingInvoker:
        async def invoke(self, **_kwargs):
            raise RuntimeError("runtime failed")

        def invoke_sync(self, **_kwargs):
            raise RuntimeError("runtime failed")

    runner = OverlaySnapshotRunner(manager, invoker=_FailingInvoker())
    request = OverlayShellRequest(
        request_id="request-a",
        command=("bash", "-lc", "true"),
        cwd=".",
        env={},
        timeout_seconds=5,
    )

    with pytest.raises(RuntimeError, match="runtime failed"):
        await runner.shell(request)

    assert manager.pinned_layers() == ()


@pytest.mark.asyncio
async def test_overlay_run_handler_supports_layer_stack_snapshot_requests(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="value.txt",
                source_path=_source(tmp_path, "value.txt", b"old\n"),
            )
        ]
    )

    result = await dispatch_envelope_async(
        {
            "op": "overlay.run",
            "args": {
                "layer_stack_root": str(manager.storage_root),
                "request_id": "handler-request",
                "command": ["bash", "-lc", "printf new > value.txt"],
                "cwd": ".",
                "env": {},
                "timeout_seconds": 5,
            },
        }
    )

    assert result["exit_code"] == 0
    assert result["snapshot_version"] == 1
    assert manager.read_text("value.txt") == ("old\n", True)
    changes = result["changes"]
    assert len(changes) == 1
    assert changes[0]["path"] == "value.txt"
    assert changes[0]["kind"] == "write"
    assert Path(changes[0]["content_path"]).read_bytes() == b"new"
    assert changes[0]["final_hash"] == hashlib.sha256(b"new").hexdigest()
