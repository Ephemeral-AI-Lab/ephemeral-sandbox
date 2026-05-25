"""Unit tests for namespace_entrypoint payload parsing."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.overlay.namespace_entrypoint import (
    WorkspaceMountMode,
    _overlay_mount_request,
    _workspace_mount_mode,
)


def _base_payload(tmp_path: Path, **overrides: object) -> dict:
    base = {
        "workspace_root": "/testbed",
        "layer_paths": ["/storage/L1", "/storage/L2"],
        "upperdir": str(tmp_path / "upper"),
        "workdir": str(tmp_path / "work"),
        "stdout_ref": str(tmp_path / "stdout.bin"),
        "stderr_ref": str(tmp_path / "stderr.bin"),
        "timings_ref": str(tmp_path / "timings.json"),
    }
    base.update(overrides)
    return base


def test_overlay_mount_request_parses_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _overlay_mount_request(payload)

    assert request.layer_paths == (Path("/storage/L1"), Path("/storage/L2"))


def test_overlay_mount_request_workspace_root_is_path(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _overlay_mount_request(payload)

    assert request.workspace_root == Path("/testbed")


def test_overlay_mount_request_raises_on_missing_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    payload.pop("layer_paths")

    with pytest.raises(KeyError):
        _overlay_mount_request(payload)


def test_overlay_mount_request_raises_on_empty_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path, layer_paths=[])

    with pytest.raises(ValueError, match="non-empty list"):
        _overlay_mount_request(payload)


def test_overlay_mount_request_raises_on_non_list_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path, layer_paths="/storage/L1")

    with pytest.raises(ValueError, match="non-empty list"):
        _overlay_mount_request(payload)


def test_overlay_mount_request_no_lowerdir_attribute(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _overlay_mount_request(payload)

    assert not hasattr(request, "lowerdir")


def test_workspace_mount_mode_requires_explicit_mode(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)

    with pytest.raises(ValueError, match="workspace_mount_mode"):
        _workspace_mount_mode(payload)


def test_workspace_mount_mode_parses_existing_mount(tmp_path: Path) -> None:
    payload = _base_payload(
        tmp_path,
        workspace_mount_mode=WorkspaceMountMode.EXISTING_MOUNT,
    )

    assert _workspace_mount_mode(payload) is WorkspaceMountMode.EXISTING_MOUNT
