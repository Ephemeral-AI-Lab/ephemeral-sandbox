"""Unit contracts for the foreground ephemeral workspace pipeline."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
from sandbox.occ.changeset import ChangesetResult, FileResult, FileStatus
from sandbox.overlay.path_change import OverlayPathChange, content_hash


class _Manifest:
    version = 1
    layers = ()


class _Snapshot:
    lease_id = "lease-1"
    manifest_version = 1
    root_hash = "root"
    manifest = _Manifest()

    def __init__(self, tmp_path: Path) -> None:
        self.layer_paths = ((tmp_path / "lower").as_posix(),)


class _LayerStack:
    storage_root: Path

    def __init__(self, tmp_path: Path, order: list[str]) -> None:
        self.storage_root = tmp_path
        self._tmp_path = tmp_path
        self._order = order
        (tmp_path / "lower").mkdir(exist_ok=True)

    def acquire_snapshot(self, *, request_id: str) -> _Snapshot:
        assert request_id.startswith("overlay:")
        self._order.append("acquire")
        return _Snapshot(self._tmp_path)

    def release_lease(self, *, lease_id: str) -> bool:
        self._order.append(f"release:{lease_id}")
        return True

    def read_active_manifest(self) -> _Manifest:
        return _Manifest()


class _DeepManifest:
    version = 1

    def __init__(self, depth: int) -> None:
        self.layers = tuple(f"layer-{index}" for index in range(depth))


class _DeepSnapshot:
    lease_id = "lease-1"
    manifest_version = 1
    root_hash = "root"

    def __init__(self, tmp_path: Path, depth: int) -> None:
        self.manifest = _DeepManifest(depth)
        self.layer_paths = tuple((tmp_path / f"lower-{index}").as_posix() for index in range(depth))


class _DeepLayerStack:
    storage_root: Path

    def __init__(self, tmp_path: Path, order: list[str], depth: int) -> None:
        self.storage_root = tmp_path
        self._tmp_path = tmp_path
        self._order = order
        self.depth = depth
        self.squash_depths: list[int] = []
        for index in range(depth):
            (tmp_path / f"lower-{index}").mkdir(exist_ok=True)

    def acquire_snapshot(self, *, request_id: str) -> _DeepSnapshot:
        assert request_id.startswith("overlay:")
        self._order.append("acquire")
        return _DeepSnapshot(self._tmp_path, self.depth)

    def release_lease(self, *, lease_id: str) -> bool:
        self._order.append(f"release:{lease_id}")
        return True

    def read_active_manifest(self) -> _DeepManifest:
        return _DeepManifest(self.depth)

    def squash(self, *, max_depth: int) -> _DeepManifest:
        self.squash_depths.append(max_depth)
        self.depth = 2
        self._order.append(f"squash:{max_depth}")
        return _DeepManifest(self.depth)


class _Occ:
    def __init__(self, order: list[str]) -> None:
        self.order = order
        self.sources: list[str] = []
        self._apply_count = 0

    async def apply_changeset(self, changes, **_kwargs) -> ChangesetResult:
        self.order.append("commit")
        self.sources.extend(change.source for change in changes)
        self._apply_count += 1
        if self._apply_count == 1:
            return ChangesetResult(
                files=(FileResult(path="shared.txt", status=FileStatus.COMMITTED),),
                published_manifest_version=2,
            )
        return ChangesetResult(
            files=(
                FileResult(
                    path="shared.txt",
                    status=FileStatus.ABORTED_VERSION,
                    message="base manifest is stale",
                ),
            ),
            published_manifest_version=None,
        )

    async def run_maintenance_after_publish(self, *_args, **_kwargs) -> dict[str, float]:
        self.order.append("maintenance")
        return {}


def _write_change(tmp_path: Path, path: str = "shared.txt") -> OverlayPathChange:
    content = tmp_path / f"{path}.content"
    content.parent.mkdir(parents=True, exist_ok=True)
    content.write_text("new\n", encoding="utf-8")
    return OverlayPathChange(
        path=path,
        kind="write",
        content_path=content.as_posix(),
        final_hash=content_hash(content),
    )


def _request(
    *,
    invocation_id: str,
    intent: Intent,
    verb: str = "write_file",
    path: str = "shared.txt",
) -> ToolCallRequest:
    args: dict[str, Any]
    if verb == "read_file":
        args = {"path": path}
    else:
        args = {"path": path, "content": "new\n"}
    return ToolCallRequest(
        invocation_id=invocation_id,
        agent_id="agent-a",
        verb=verb,
        intent=intent,
        args=args,
    )


def test_operation_overlay_release_uses_daemon_lease_guard(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []
    writable_root = tmp_path / "writable"
    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: writable_root,
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: writable_root,
    )
    pipeline = EphemeralPipeline(
        occ_client=_Occ(order),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(tmp_path, order),
    )

    handle = pipeline.acquire_operation_overlay(
        invocation_id="overlay:plugin-write",
        workspace_root="/testbed",
    )

    assert "lease-1" not in pipeline._lease_guard._released_lease_ids
    assert handle.run_dir.exists()

    handle.release()
    handle.release()

    assert order == ["acquire", "release:lease-1"]
    assert pipeline._lease_guard._released_lease_ids == {"lease-1"}
    # Sync release() is lease-only; release_overlay() owns scratch rmtree, so
    # run_dir persists past a bare handle.release().
    assert handle.run_dir.exists()


@pytest.mark.asyncio
async def test_ephemeral_write_acquire_run_capture_commit_destroy_order(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []
    occ = _Occ(order)

    async def fake_run(_handle, req):
        assert req.intent is Intent.WRITE_ALLOWED
        order.append("run")
        return {"success": True, "status": "ok", "timings": {}}

    async def fake_capture(_handle):
        order.append("capture")
        return [_write_change(tmp_path)]

    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr("sandbox.ephemeral_workspace.pipeline.run_in_namespace", fake_run)
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        fake_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=occ,
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(tmp_path, order),
    )

    result = await pipeline.run_tool_call(
        _request(invocation_id="req-write", intent=Intent.WRITE_ALLOWED)
    )

    assert result["success"] is True
    assert result["changed_paths"] == ["shared.txt"]
    assert occ.sources == ["api_write"]
    assert order == [
        "acquire",
        "run",
        "capture",
        "commit",
        "maintenance",
        "release:lease-1",
    ]


@pytest.mark.asyncio
async def test_ephemeral_read_skips_commit_but_still_destroys(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []

    async def fake_run(_handle, req):
        assert req.intent is Intent.READ_ONLY
        order.append("run")
        return {"success": True, "content": "ok\n", "timings": {}}

    async def fail_capture(_handle):
        raise AssertionError("read-only requests must not capture upperdir")

    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr("sandbox.ephemeral_workspace.pipeline.run_in_namespace", fake_run)
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        fail_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=_Occ(order),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(tmp_path, order),
    )

    result = await pipeline.run_tool_call(
        _request(
            invocation_id="req-read",
            intent=Intent.READ_ONLY,
            verb="read_file",
        )
    )

    assert result["success"] is True
    assert result["content"] == "ok\n"
    assert order == ["acquire", "run", "release:lease-1"]


@pytest.mark.asyncio
async def test_shell_pre_mount_squashes_deep_manifest(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []

    async def fake_run(_handle, req):
        assert req.verb == "shell"
        order.append("run")
        return {"success": True, "status": "ok", "timings": {}}

    monkeypatch.setenv("EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH", "4")
    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr("sandbox.ephemeral_workspace.pipeline.run_in_namespace", fake_run)
    stack = _DeepLayerStack(tmp_path, order, depth=8)
    pipeline = EphemeralPipeline(
        occ_client=_Occ(order),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=stack,
    )

    result = await pipeline.run_tool_call(
        ToolCallRequest(
            invocation_id="req-shell",
            agent_id="agent-a",
            verb="shell",
            intent=Intent.READ_ONLY,
            args={"command": "true"},
        )
    )

    assert result["success"] is True
    assert stack.squash_depths == [4]
    assert order == ["squash:4", "acquire", "run", "release:lease-1"]
    timings = result["timings"]
    assert timings["layer_stack.shell_pre_mount_squash.max_depth"] == 4.0
    assert timings["layer_stack.shell_pre_mount_squash.depth_before"] == 8.0
    assert timings["layer_stack.shell_pre_mount_squash.depth_after"] == 2.0
    assert timings["layer_stack.shell_pre_mount_squash.total_s"] >= 0.0


@pytest.mark.asyncio
async def test_ephemeral_same_path_concurrent_conflict_is_typed(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    order: list[str] = []

    async def fake_run(_handle, _req):
        await asyncio.sleep(0)
        return {"success": True, "status": "ok", "timings": {}}

    async def fake_capture(_handle):
        return [_write_change(tmp_path)]

    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr("sandbox.ephemeral_workspace.pipeline.run_in_namespace", fake_run)
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        fake_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=_Occ(order),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(tmp_path, order),
    )

    first, second = await asyncio.gather(
        pipeline.run_tool_call(
            _request(invocation_id="req-1", intent=Intent.WRITE_ALLOWED)
        ),
        pipeline.run_tool_call(
            _request(invocation_id="req-2", intent=Intent.WRITE_ALLOWED)
        ),
    )

    results = sorted((first, second), key=lambda item: bool(item.get("success")))
    conflict = results[0]
    success = results[1]
    assert success["success"] is True
    assert conflict["success"] is False
    assert conflict["status"] == "aborted_version"
    assert conflict["conflict"] == {
        "reason": "aborted_version",
        "conflict_file": "shared.txt",
        "message": "base manifest is stale",
    }


@pytest.mark.asyncio
async def test_out_of_workspace_paths_use_same_pipeline(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: list[tuple[str, str]] = []

    async def fake_run(_handle, req):
        seen.append((req.verb, str(req.args.get("path"))))
        return {"success": True, "status": "ok", "timings": {}}

    async def fake_capture(_handle):
        return [_write_change(tmp_path, "tmp/scratch")]

    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr("sandbox.ephemeral_workspace.pipeline.run_in_namespace", fake_run)
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        fake_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=_Occ([]),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(tmp_path, []),
    )

    read_result = await pipeline.run_tool_call(
        _request(
            invocation_id="req-read-host",
            intent=Intent.READ_ONLY,
            verb="read_file",
            path="/etc/hosts",
        )
    )
    write_result = await pipeline.run_tool_call(
        _request(
            invocation_id="req-write-tmp",
            intent=Intent.WRITE_ALLOWED,
            path="/tmp/scratch",
        )
    )

    assert read_result["success"] is True
    assert write_result["success"] is True
    assert seen == [("read_file", "/etc/hosts"), ("write_file", "/tmp/scratch")]

    from sandbox.overlay.namespace_entrypoint import execute_tool_payload

    denied = execute_tool_payload(
        {
            "workspace_root": tmp_path.as_posix(),
            "tool_call": _request(
                invocation_id="req-deny",
                intent=Intent.WRITE_ALLOWED,
                path="/etc/hosts",
            ).to_payload(),
            "stdout_ref": (tmp_path / "stdout").as_posix(),
            "stderr_ref": (tmp_path / "stderr").as_posix(),
        }
    )
    assert denied["success"] is False
    assert denied["error"]["kind"] == "forbidden_host_path"
