"""Tests for ``sandbox.api.shell``."""

from __future__ import annotations

from pathlib import Path

from sandbox.api import RequestActor, ShellRequest
from sandbox.api.shell import shell
from sandbox.layer_stack import LayerStackManager
from sandbox.occ.client import dispose_occ_service, register_occ_service
from sandbox.occ.service import OccService
from sandbox.overlay.client import (
    OverlayClient,
    dispose_overlay_client,
    register_overlay_client,
)
from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner


class _Gitignore:
    def is_ignored(self, path: str) -> bool:
        del path
        return False


async def test_shell_routes_through_overlay_capture_and_occ_commit(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    register_occ_service(
        "sb-shell-cutover",
        OccService(gitignore=_Gitignore(), layer_stack=manager),
    )
    register_overlay_client(
        "sb-shell-cutover",
        OverlayClient(runner=SnapshotOverlayRunner(manager)),
    )
    try:
        result = await shell(
            "sb-shell-cutover",
            ShellRequest(
                command="mkdir -p pkg; printf 'new\\n' > pkg/value.txt; cat pkg/value.txt",
                cwd=".",
                timeout=12,
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_overlay_client("sb-shell-cutover")
        dispose_occ_service("sb-shell-cutover")

    assert result.success is True
    assert result.status == "ok"
    assert result.exit_code == 0
    assert result.stdout == "new\n"
    assert result.stderr == ""
    assert result.changed_paths == ("pkg/value.txt",)
    assert manager.read_text("pkg/value.txt") == ("new\n", True)


async def test_shell_fails_closed_without_overlay_binding() -> None:
    result = await shell(
        "sb-shell-unbound",
        ShellRequest(
            command="cat pyproject.toml | tee copied.txt",
            cwd="/workspace",
            actor=RequestActor(agent_id="agent-1"),
        ),
    )

    assert result.success is False
    assert result.exit_code == 1
    assert result.status == "error"
    assert result.changed_paths == ()
    assert result.conflict is not None
    assert result.conflict.reason == "MissingOverlayClient"
    assert result.conflict_reason == (
        "no typed overlay client is registered for sandbox 'sb-shell-unbound'"
    )
    assert result.warnings == ()
