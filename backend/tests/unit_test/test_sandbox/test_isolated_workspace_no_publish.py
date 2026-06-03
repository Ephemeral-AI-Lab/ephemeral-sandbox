"""Isolated workspace tool-call publish boundary tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.isolated_workspace import IsolatedWorkspaceHandle, IsolatedPipeline
from sandbox.overlay.path_change import OverlayPathChange, content_hash


class _LayerStack:
    def apply_changeset(self, *_args, **_kwargs) -> None:
        raise AssertionError("isolated workspace writes must never publish OCC")


@pytest.mark.asyncio
async def test_iws_tool_call_never_publishes_occ(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    content = tmp_path / "content.txt"
    content.write_text("new\n", encoding="utf-8")
    captured = [
        OverlayPathChange(
            path="iws.txt",
            kind="write",
            content_path=content.as_posix(),
            final_hash=content_hash(content),
        )
    ]

    async def fake_run(_handle, req, *, isolated_runner):
        del isolated_runner
        assert req.intent is Intent.WRITE_ALLOWED
        return {"success": True, "status": "ok", "timings": {}}

    async def fake_capture(_handle):
        return captured

    monkeypatch.setattr("sandbox.isolated_workspace.pipeline.run_in_namespace", fake_run)
    monkeypatch.setattr(
        "sandbox.isolated_workspace.pipeline.overlay_lifecycle.capture_changes",
        fake_capture,
    )
    pipeline = IsolatedPipeline(scratch_root=tmp_path, layer_stack=_LayerStack())
    handle = IsolatedWorkspaceHandle(
        workspace_handle_id="h1",
        agent_id="agent-a",
        lease_id="lease-iws",
        manifest_version=1,
        manifest_root_hash="root",
        workspace_root="/testbed",
        scratch_dir=tmp_path / "scratch",
        upperdir=tmp_path / "scratch" / "upper",
        workdir=tmp_path / "scratch" / "work",
        holder_pid=1234,
    )
    handle.upperdir.mkdir(parents=True)
    handle.workdir.mkdir(parents=True)
    pipeline._handles[handle.workspace_handle_id] = handle
    pipeline._by_agent[handle.agent_id] = handle.workspace_handle_id

    result = await pipeline.run_tool_call(
        ToolCallRequest(
            invocation_id="req-write",
            agent_id="agent-a",
            verb="write_file",
            intent=Intent.WRITE_ALLOWED,
            args={"path": "iws.txt", "content": "new\n"},
        )
    )

    assert result["success"] is True
    assert result["workspace"] == "isolated"
    assert result["changed_paths"] == ["iws.txt"]
