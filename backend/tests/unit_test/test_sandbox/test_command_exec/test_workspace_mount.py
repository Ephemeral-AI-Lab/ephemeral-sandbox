"""Namespace overlay mount behavior tests."""

from __future__ import annotations

from pathlib import Path

from sandbox._shared.command_exec_contract import CommandExecRequest
from sandbox.overlay import kernel_mount


def test_command_request_is_namespace_only() -> None:
    request = CommandExecRequest(
        invocation_id="req-1",
        workspace_ref="/tmp/stack",
        workspace_root="/testbed",
        command=("bash", "-lc", "printf ok"),
    )
    assert request.workspace_root == "/testbed"
    assert request.command == ("bash", "-lc", "printf ok")


def test_namespace_mount_validation_keeps_real_mountpoint_and_fd_backed_layers(
    tmp_path: Path,
) -> None:
    workspace_root = tmp_path / "workspace"
    layer1 = tmp_path / "layer1"
    layer2 = tmp_path / "layer2"
    upperdir = tmp_path / "upper"
    workdir = tmp_path / "work"
    workspace_root.mkdir()
    layer1.mkdir()
    layer2.mkdir()

    inputs = kernel_mount.validate_mount_inputs(
        workspace_root=workspace_root,
        layer_paths=(layer1, layer2),
        upperdir=upperdir,
        workdir=workdir,
    )
    try:
        assert inputs.workspace_root == workspace_root
        assert len(inputs.layer_paths) == 2
        assert all(p.as_posix().startswith("/proc/self/fd/") for p in inputs.layer_paths)
        assert inputs.upperdir == upperdir
        assert inputs.workdir == workdir
    finally:
        inputs.close()
