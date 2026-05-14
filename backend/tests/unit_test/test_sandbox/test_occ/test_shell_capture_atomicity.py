"""Phase 05 — shell capture continues to reach OCC via OccClient (not OccService).

Phase 04 established that shell capture submits typed changes via
``OccClient.apply_changeset`` rather than calling ``OccService`` directly.
Phase 05 §6 re-confirms this: the simplified plan §"Shared OCC Publish
Gate" forbids bypassing the OCC client boundary from capture conversion.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.execution.contract import ShellProcessResult
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.occ.client import OccClient
from sandbox.daemon.service import occ_backend, shell_runner


@pytest.mark.asyncio
async def test_shell_uses_occ_client_apply_changeset(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The shell handler must call OccClient.apply_changeset (not bypass it)."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "input.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    services = shell_runner.services({"layer_stack_root": stack.as_posix()})
    occ_client = services[1]
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
    ):
        apply_calls.append(
            {
                "changes": tuple(typed_changes),
                "snapshot": snapshot,
                "options": options,
                "workspace_ref": workspace_ref,
            }
        )
        return await real_apply(
            typed_changes,
            snapshot=snapshot,
            options=options,
            workspace_ref=workspace_ref,
        )

    monkeypatch.setattr(occ_client, "apply_changeset", tracking_apply)

    def fake_run(*, spec, request, run_dir, timings):
        del request
        upper = Path(spec.upperdir)
        upper.mkdir(parents=True, exist_ok=True)
        out = upper / "out.txt"
        out.write_text("shell wrote me\n", encoding="utf-8")
        stdout_ref = Path(run_dir) / "stdout.bin"
        stderr_ref = Path(run_dir) / "stderr.bin"
        stdout_ref.write_text("ok\n", encoding="utf-8")
        stderr_ref.write_text("", encoding="utf-8")
        timings["command_exec.mount_workspace_s"] = 0.0
        timings["command_exec.run_command_s"] = 0.0
        return ShellProcessResult(
            exit_code=0,
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=spec.workspace_root,
            mount_mode="private_namespace",
        )

    monkeypatch.setattr(shell_runner, "run_workspace_replaced_command", fake_run)

    result = await shell_runner.execute_shell_api(
        {
            "layer_stack_root": stack.as_posix(),
            "command": "true",
            "cwd": ".",
            "actor_id": "agent-1",
            "description": "shell-capture",
        }
    )

    assert result["success"] is True
    assert apply_calls, "shell must invoke OccClient.apply_changeset"
    # The shell-capture call carries the leased manifest as its snapshot —
    # confirms OCC sees the leased identity, not a fresh active read.
    submitted = apply_calls[0]
    assert submitted["snapshot"] is not None
