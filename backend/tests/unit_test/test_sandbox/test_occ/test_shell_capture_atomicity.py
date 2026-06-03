"""Phase 05 — shell capture continues to reach OCC via OccClient (not OccService).

Phase 04 established that shell capture submits typed changes via
``OccClient.apply_changeset`` rather than calling ``OccService`` directly.
Phase 05 §6 re-confirms this: the simplified plan §"Shared OCC Publish
Gate" forbids bypassing the OCC client boundary from capture conversion.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.occ.client import OccClient
from sandbox.overlay.path_change import OverlayPathChange, content_hash
import sandbox.overlay.writable_dirs as writable_dirs_mod
from sandbox.daemon import occ_runtime_services
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
import sandbox.ephemeral_workspace.pipeline as pipeline_mod


@pytest.mark.asyncio
async def test_shell_uses_occ_client_apply_changeset(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The shell handler must call OccClient.apply_changeset (not bypass it)."""
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "input.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    backend = occ_runtime_services.get_occ_runtime_services(stack.as_posix())
    occ_client = backend.occ_client
    assert isinstance(occ_client, OccClient), (
        "Shell capture must reach OCC through OccClient — direct OccService "
        "binding is forbidden by the plan §Shared OCC Publish Gate."
    )

    apply_calls: list[dict] = []
    real_apply = occ_client.apply_changeset

    async def tracking_apply(
        typed_changes,
        *,
        snapshot=None,
        options=None,
        workspace_ref=None,
        run_maintenance=True,
    ):
        apply_calls.append(
            {
                "changes": tuple(typed_changes),
                "snapshot": snapshot,
                "options": options,
                "workspace_ref": workspace_ref,
                "run_maintenance": run_maintenance,
            }
        )
        return await real_apply(
            typed_changes,
            snapshot=snapshot,
            options=options,
            workspace_ref=workspace_ref,
            run_maintenance=run_maintenance,
        )

    monkeypatch.setattr(occ_client, "apply_changeset", tracking_apply)

    captured = tmp_path / "capture" / "out.txt"
    captured.parent.mkdir()
    captured.write_text("shell wrote me\n", encoding="utf-8")
    writable_root = tmp_path / "overlay-writable-root"
    writable_root.mkdir()
    monkeypatch.setattr(writable_dirs_mod, "OVERLAY_WRITABLE_ROOT", writable_root)

    async def fake_run(handle, req):
        del handle, req
        return {
            "success": True,
            "status": "ok",
            "exit_code": 0,
            "stdout": "ok\n",
            "stderr": "",
            "timings": {},
        }

    async def fake_capture(handle):
        del handle
        return [
            OverlayPathChange(
                path="out.txt",
                kind="write",
                content_path=captured.as_posix(),
                final_hash=content_hash(captured),
            )
        ]

    monkeypatch.setattr(pipeline_mod, "run_in_namespace", fake_run)
    monkeypatch.setattr(
        pipeline_mod.overlay_lifecycle,
        "capture_changes",
        fake_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=occ_client,
        workspace_ref=stack.as_posix(),
        layer_stack=backend.layer_stack,
        workspace_root=workspace.as_posix(),
    )

    result = await pipeline.run_tool_call(
        ToolCallRequest(
            invocation_id="shell-capture",
            agent_id="agent-1",
            verb="shell",
            intent=Intent.WRITE_ALLOWED,
            args={"command": "true", "cwd": "."},
        )
    )

    assert result["success"] is True
    assert apply_calls, "shell must invoke OccClient.apply_changeset"
    # The shell-capture call carries the leased manifest as its snapshot —
    # confirms OCC sees the leased identity, not a fresh active read.
    submitted = apply_calls[0]
    assert submitted["snapshot"] is not None
