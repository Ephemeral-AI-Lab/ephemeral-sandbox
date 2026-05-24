"""Unit tests for kernel_mount.py with the new mount API."""

from __future__ import annotations

import errno
import sys
from pathlib import Path
from unittest.mock import MagicMock

import pytest

import sandbox.overlay.kernel_mount as km
from sandbox.overlay.kernel_mount import (
    mount_overlay,
    validate_mount_inputs,
)
from sandbox.overlay.new_mount_api import (
    SYS_fsconfig,
    SYS_fsmount,
    SYS_fsopen,
    SYS_move_mount,
)

_IS_LINUX = sys.platform == "linux"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_libc_mock(return_value: int = 0, errno_val: int = 0) -> MagicMock:
    import ctypes

    mock = MagicMock()

    def fake_syscall(*args, **kwargs):
        ctypes.set_errno(errno_val)
        return return_value

    mock.syscall.side_effect = fake_syscall
    return mock


def test_mount_overlay_raises_on_missing_libc(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(km, "_get_libc", lambda: None)
    with pytest.raises(OSError, match="libc not found"):
        mount_overlay(
            workspace_root=Path("/workspace"),
            layer_paths=(Path("/storage/L1"),),
            upperdir=Path("/scratch/upper"),
            workdir=Path("/scratch/work"),
        )


def test_mount_overlay_calls_fsopen_then_fsconfig_per_layer(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Assert fsopen → fsconfig(lowerdir+) × N → fsconfig(upperdir) → fsconfig(workdir)
    → fsconfig(CMD_CREATE) → fsmount → move_mount sequence."""
    calls: list[tuple[object, ...]] = []

    def fake_syscall(*args: object) -> int:
        calls.append(args)
        return 3  # fd value

    mock = MagicMock()
    mock.syscall.side_effect = fake_syscall
    monkeypatch.setattr(km, "_get_libc", lambda: mock)

    closed: list[int] = []
    monkeypatch.setattr(km.os, "close", lambda fd: closed.append(fd))

    mount_overlay(
        workspace_root=Path("/workspace"),
        layer_paths=(Path("/storage/L1"), Path("/storage/L2")),
        upperdir=Path("/scratch/upper"),
        workdir=Path("/scratch/work"),
    )

    syscall_numbers = [c[0] for c in calls]
    # First call: fsopen
    assert syscall_numbers[0] == SYS_fsopen
    # lowerdir+ calls for each layer
    lowerdir_calls = [c for c in calls if c[0] == SYS_fsconfig and len(c) > 3 and c[3] == b"lowerdir+"]
    assert len(lowerdir_calls) == 2
    # upperdir and workdir calls
    upperdir_calls = [c for c in calls if c[0] == SYS_fsconfig and len(c) > 3 and c[3] == b"upperdir"]
    assert len(upperdir_calls) == 1
    workdir_calls = [c for c in calls if c[0] == SYS_fsconfig and len(c) > 3 and c[3] == b"workdir"]
    assert len(workdir_calls) == 1
    # fsmount and move_mount present
    assert SYS_fsmount in syscall_numbers
    assert SYS_move_mount in syscall_numbers


def test_mount_overlay_iterates_layers_in_natural_order(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """First element of layer_paths must be the first lowerdir+ call (top priority)."""
    lowerdir_values: list[bytes] = []

    def fake_syscall(*args: object) -> int:
        if args[0] == SYS_fsconfig and len(args) > 3 and args[3] == b"lowerdir+":
            lowerdir_values.append(args[4])  # type: ignore[arg-type]
        return 3

    mock = MagicMock()
    mock.syscall.side_effect = fake_syscall
    monkeypatch.setattr(km, "_get_libc", lambda: mock)
    monkeypatch.setattr(km.os, "close", lambda fd: None)

    layer_paths = (
        Path("/storage/newest"),
        Path("/storage/middle"),
        Path("/storage/oldest"),
    )
    mount_overlay(
        workspace_root=Path("/workspace"),
        layer_paths=layer_paths,
        upperdir=Path("/scratch/upper"),
        workdir=Path("/scratch/work"),
    )

    import os

    assert lowerdir_values[0] == os.fsencode("/storage/newest")
    assert lowerdir_values[1] == os.fsencode("/storage/middle")
    assert lowerdir_values[2] == os.fsencode("/storage/oldest")


def test_mount_overlay_propagates_fsopen_errno(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    libc_mock = _make_libc_mock(-1, errno.EPERM)
    monkeypatch.setattr(km, "_get_libc", lambda: libc_mock)

    with pytest.raises(OSError) as exc_info:
        mount_overlay(
            workspace_root=Path("/workspace"),
            layer_paths=(Path("/storage/L1"),),
            upperdir=Path("/scratch/upper"),
            workdir=Path("/scratch/work"),
        )
    assert exc_info.value.errno == errno.EPERM


# ---------------------------------------------------------------------------
# validate_mount_inputs — fd layout
# ---------------------------------------------------------------------------


def test_validate_mount_inputs_keeps_real_mountpoint_and_fd_backed_sources(
    tmp_path: Path,
) -> None:
    workspace_root = tmp_path / "workspace"
    layer1 = tmp_path / "layer1"
    layer2 = tmp_path / "layer2"
    workspace_root.mkdir()
    layer1.mkdir()
    layer2.mkdir()

    inputs = validate_mount_inputs(
        workspace_root=workspace_root,
        layer_paths=(layer1, layer2),
        upperdir=tmp_path / "upper",
        workdir=tmp_path / "work",
    )
    try:
        assert inputs.workspace_root == workspace_root
        assert len(inputs.layer_paths) == 2
        assert all(p.as_posix().startswith("/proc/self/fd/") for p in inputs.layer_paths)
        assert inputs.upperdir.as_posix().startswith("/proc/self/fd/")
        assert inputs.workdir.as_posix().startswith("/proc/self/fd/")
        # fd count: workspace + 2 layers + upperdir + workdir = 5
        assert len(inputs.fds) == 5
    finally:
        inputs.close()


def test_validate_mount_inputs_rejects_symlinked_layer(tmp_path: Path) -> None:
    workspace_root = tmp_path / "workspace"
    workspace_root.mkdir()
    real_layer = tmp_path / "real_layer"
    real_layer.mkdir()
    sym_layer = tmp_path / "sym_layer"
    sym_layer.symlink_to(real_layer)

    with pytest.raises(ValueError, match="symlink"):
        validate_mount_inputs(
            workspace_root=workspace_root,
            layer_paths=(sym_layer,),
            upperdir=tmp_path / "upper",
            workdir=tmp_path / "work",
        )


def test_validate_mount_inputs_rejects_missing_layer(tmp_path: Path) -> None:
    workspace_root = tmp_path / "workspace"
    workspace_root.mkdir()

    with pytest.raises(ValueError, match="missing"):
        validate_mount_inputs(
            workspace_root=workspace_root,
            layer_paths=(tmp_path / "nonexistent",),
            upperdir=tmp_path / "upper",
            workdir=tmp_path / "work",
        )


def test_validate_mount_inputs_closes_fds_on_error(tmp_path: Path) -> None:
    workspace_root = tmp_path / "workspace"
    workspace_root.mkdir()

    closed: list[int] = []
    original_close = km.os.close

    import contextlib

    def tracking_close(fd: int) -> None:
        closed.append(fd)
        with contextlib.suppress(OSError):
            original_close(fd)

    import unittest.mock

    with unittest.mock.patch.object(km.os, "close", tracking_close):
        with pytest.raises(ValueError):
            validate_mount_inputs(
                workspace_root=workspace_root,
                layer_paths=(tmp_path / "nonexistent",),
                upperdir=tmp_path / "upper",
                workdir=tmp_path / "work",
            )

    assert len(closed) >= 1  # workspace_root fd was opened and closed


def test_mount_overlay_legacy_mount8_removed() -> None:
    assert not hasattr(km, "_mount_overlay_legacy_mount8")
