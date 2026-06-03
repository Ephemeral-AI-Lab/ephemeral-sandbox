"""Focused Phase 2 workspace-unification checks."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox._shared.tool_primitives import VERB_TABLE
from sandbox.daemon.rpc import dispatcher
from sandbox.occ.overlay_change_conversion import overlay_path_changes_to_occ_changes
from sandbox.occ.changeset import ChangesetResult, FileResult, FileStatus
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.lifecycle import release_overlay
from sandbox.overlay.namespace_entrypoint import execute_tool_payload
from sandbox.overlay.path_change import OverlayPathChange, content_hash
import sandbox.overlay.writable_dirs as writable_dirs_mod
from sandbox.isolated_workspace import IsolatedWorkspaceHandle, IsolatedPipeline


def test_verb_table_excludes_shell() -> None:
    assert sorted(VERB_TABLE) == [
        "edit_file",
        "glob",
        "grep",
        "read_file",
        "write_file",
    ]


def test_namespace_entrypoint_uses_verb_table_for_uniform_verbs(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "hello.txt").write_text("hi\n", encoding="utf-8")
    req = ToolCallRequest(
        invocation_id="r1",
        agent_id="agent",
        verb="read_file",
        intent=Intent.READ_ONLY,
        args={"path": "hello.txt"},
    )

    result = execute_tool_payload(
        {
            "workspace_root": workspace.as_posix(),
            "tool_call": req.to_payload(),
            "stdout_ref": (tmp_path / "stdout").as_posix(),
            "stderr_ref": (tmp_path / "stderr").as_posix(),
        }
    )

    assert result["success"] is True
    assert result["content"] == "hi\n"


def test_namespace_entrypoint_blocks_write_to_host_denylist(tmp_path: Path) -> None:
    req = ToolCallRequest(
        invocation_id="r1",
        agent_id="agent",
        verb="write_file",
        intent=Intent.WRITE_ALLOWED,
        args={"path": "/etc/hosts", "content": "bad"},
    )

    result = execute_tool_payload(
        {
            "workspace_root": tmp_path.as_posix(),
            "tool_call": req.to_payload(),
            "stdout_ref": (tmp_path / "stdout").as_posix(),
            "stderr_ref": (tmp_path / "stderr").as_posix(),
        }
    )

    assert result["success"] is False
    assert result["error"]["kind"] == "forbidden_host_path"


def test_overlay_change_conversion_threads_source_to_all_change_kinds(
    tmp_path: Path,
) -> None:
    content = tmp_path / "content.txt"
    content.write_text("new\n", encoding="utf-8")
    link = tmp_path / "link-target"
    os.symlink("target.txt", link)

    changes = overlay_path_changes_to_occ_changes(
        [
            OverlayPathChange(
                path="a.txt",
                kind="write",
                content_path=content.as_posix(),
                final_hash=content_hash(content),
            ),
            OverlayPathChange(path="b.txt", kind="delete", content_path=None, final_hash=None),
            OverlayPathChange(
                path="c.txt",
                kind="symlink",
                content_path=link.as_posix(),
                final_hash=content_hash(link, symlink=True),
            ),
            OverlayPathChange(path="dir", kind="opaque_dir", content_path=None, final_hash=None),
        ],
        source="api_write",
    )

    assert {change.source for change in changes} == {"api_write"}


def test_overlay_release_is_idempotent_under_concurrent_calls(tmp_path: Path) -> None:
    run_dir = tmp_path / "run"
    upper = run_dir / "upper"
    work = run_dir / "work"
    upper.mkdir(parents=True)
    work.mkdir()
    releases = 0

    def release() -> bool:
        nonlocal releases
        releases += 1
        return True

    handle = OverlayHandle(
        workspace_root="/testbed",
        layer_paths=(),
        upperdir=upper,
        workdir=work,
        lease_id="lease-1",
        holder_pid=None,
        run_dir=run_dir,
        _release=release,
    )

    async def _release_twice() -> None:
        await asyncio.gather(release_overlay(handle), release_overlay(handle))

    asyncio.run(_release_twice())

    assert releases == 1
    assert handle._released is True


def test_plugin_gate_blocks_open_isolated_workspace(monkeypatch) -> None:
    class _Iws:
        @staticmethod
        def get_handle(agent_id: str) -> object | None:
            return object() if agent_id == "agent" else None

    monkeypatch.setattr(dispatcher, "get_active_pipeline", lambda: _Iws())
    blocked = dispatcher._plugin_block_decision("api.plugin.ensure", "agent")
    assert blocked is not None
    assert blocked["error"]["kind"] == "forbidden_in_isolated_workspace"

    blocked = dispatcher._plugin_block_decision("plugin.demo.run", "agent")
    assert blocked is not None
    assert blocked["error"]["kind"] == "forbidden_in_isolated_workspace"


def test_ephemeral_run_tool_call_uses_api_write_for_single_path(
    tmp_path: Path,
    monkeypatch,
) -> None:
    content = tmp_path / "content.txt"
    content.write_text("new\n", encoding="utf-8")
    captured = [
        OverlayPathChange(
            path="a.txt",
            kind="write",
            content_path=content.as_posix(),
            final_hash=content_hash(content),
        )
    ]
    seen_sources: list[str] = []

    class _Lease:
        lease_id = "lease-1"
        manifest_version = 1
        manifest = object()
        layer_paths = (tmp_path.as_posix(),)

    class _LayerStack:
        storage_root = tmp_path

        @staticmethod
        def acquire_snapshot(*, request_id: str) -> _Lease:
            return _Lease()

        @staticmethod
        def release_lease(*, lease_id: str) -> bool:
            return True

        @staticmethod
        def read_active_manifest():
            class _Manifest:
                version = 2
                layers = ()

            return _Manifest()

    class _Occ:
        async def apply_changeset(self, changes, **kwargs):
            seen_sources.extend(change.source for change in changes)
            return ChangesetResult(
                files=(FileResult(path="a.txt", status=FileStatus.COMMITTED),),
                published_manifest_version=2,
            )

        async def run_maintenance_after_publish(self, result, **kwargs):
            return {}

    async def _fake_run(handle, req):
        return {"success": True, "status": "ok", "timings": {}}

    async def _fake_capture(handle):
        return captured

    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.run_in_namespace",
        _fake_run,
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        _fake_capture,
    )
    writable_root = tmp_path / "overlay-writable-root"
    writable_root.mkdir()
    monkeypatch.setattr(writable_dirs_mod, "OVERLAY_WRITABLE_ROOT", writable_root)
    pipeline = EphemeralPipeline(
        occ_client=_Occ(),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(),
    )

    result = asyncio.run(
        pipeline.run_tool_call(
            ToolCallRequest(
                invocation_id="r1",
                agent_id="agent",
                verb="write_file",
                intent=Intent.WRITE_ALLOWED,
                args={"path": "a.txt", "content": "new\n"},
            )
        )
    )

    assert result["success"] is True
    assert result["changed_paths"] == ["a.txt"]
    assert seen_sources == ["api_write"]


def test_isolated_run_tool_call_uses_existing_handle_overlay(
    tmp_path: Path,
    monkeypatch,
) -> None:
    content = tmp_path / "iws.txt.content"
    content.write_text("new\n", encoding="utf-8")
    captured = [
        OverlayPathChange(
            path="iws.txt",
            kind="write",
            content_path=content.as_posix(),
            final_hash=content_hash(content),
        )
    ]
    seen: dict[str, object] = {}

    class _LayerStack:
        pass

    async def _fake_run(handle, req, *, isolated_runner):
        seen["workspace_root"] = handle.workspace_root
        seen["upperdir"] = handle.upperdir
        seen["workdir"] = handle.workdir
        seen["holder_pid"] = handle.holder_pid
        seen["agent_id"] = req.agent_id
        return {"success": True, "status": "ok", "timings": {}}

    async def _fake_capture(handle):
        seen["captured_upperdir"] = handle.upperdir
        return captured

    monkeypatch.setattr(
        "sandbox.isolated_workspace.pipeline.run_in_namespace",
        _fake_run,
    )
    monkeypatch.setattr(
        "sandbox.isolated_workspace.pipeline.overlay_lifecycle.capture_changes",
        _fake_capture,
    )

    pipeline = IsolatedPipeline(
        scratch_root=tmp_path,
        layer_stack=_LayerStack(),
    )
    handle = IsolatedWorkspaceHandle(
        workspace_handle_id="h1",
        agent_id="agent",
        lease_id="lease-iws",
        manifest_version=7,
        manifest_root_hash="root",
        workspace_root="/testbed",
        scratch_dir=tmp_path / "scratch",
        upperdir=tmp_path / "upper",
        workdir=tmp_path / "work",
        holder_pid=1234,
    )
    pipeline._handles[handle.workspace_handle_id] = handle
    pipeline._by_agent[handle.agent_id] = handle.workspace_handle_id

    result = asyncio.run(
        pipeline.run_tool_call(
            ToolCallRequest(
                invocation_id="r1",
                agent_id="agent",
                verb="write_file",
                intent=Intent.WRITE_ALLOWED,
                args={"path": "iws.txt", "content": "new\n"},
            )
        )
    )

    assert result["workspace"] == "isolated"
    assert result["changed_paths"] == ["iws.txt"]
    assert seen == {
        "workspace_root": "/testbed",
        "upperdir": tmp_path / "upper",
        "workdir": tmp_path / "work",
        "holder_pid": 1234,
        "agent_id": "agent",
        "captured_upperdir": tmp_path / "upper",
    }
